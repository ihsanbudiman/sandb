import { ApiError, type Backend, type ColumnInfo, type Connection, type ConnectionInput, type Engine, type StatementResult, type CellValue, type GuardCheck, type WriteInfo, type ForeignKeyInfo, type TableInfo, type TableProperties, type IndexInfo, type MongoCollectionStats, type AiSettings, type EditableResult, type Topology, type ResultSort } from './types';
import { schemas, nsOf, pkOf, colType, runMock, statementKind, tableData } from '../mock';
import { parseMongoUri } from '../mongoUri';

// In-memory backend: the prototype behaviour, used when there is neither Tauri nor the dev bridge.

const versions: Record<Engine, string> = { pg: 'PostgreSQL 16.4', mysql: 'MySQL 8.4', sqlite: 'SQLite 3.46', mongo: 'MongoDB 8.0.3' };

export const seedConnections: Connection[] = [
  { id: 'prod', name: 'prod-analytics', engine: 'pg', version: 'PostgreSQL 16.4', env: 'production', host: 'prod-analytics.db.internal', port: 5432, database: 'analytics_prod', user: 'analyst', ssl: 'require', file: '', aiWrite: false, readOnly: false, uri: '', authSource: '', topology: null },
  { id: 'shop', name: 'shop-staging', engine: 'mysql', version: 'MySQL 8.4', env: 'staging', host: 'shop-staging.db.internal', port: 3306, database: 'shop', user: 'app', ssl: 'prefer', file: '', aiWrite: false, readOnly: false, uri: '', authSource: '', topology: null },
  { id: 'notes', name: 'notes.db', engine: 'sqlite', version: 'SQLite 3.46', env: 'local', host: '', port: 0, database: 'main', user: '', ssl: 'prefer', file: '~/data/notes.db', aiWrite: false, readOnly: false, uri: '', authSource: '', topology: null },
  { id: 'mongo', name: 'local mongo', engine: 'mongo', version: 'MongoDB 8.0.3', env: 'local', host: '127.0.0.1', port: 57017, database: 'shop_staging', user: 'sandb', ssl: 'disable', file: '', aiWrite: false, readOnly: false, uri: '', authSource: 'admin', topology: 'replica_set' },
];

let list: Connection[] = seedConnections.map((c) => ({ ...c }));

// Tab sessions, as the backend keeps them: only writable Production connections open one, and only while its
// transaction holds changes that wait for Commit or Rollback.
const sessions = new Map<string, { connId: string }>();

// Rough stand-ins for the backend's statement facts; never a security boundary.
const bare = (p: string) => p.replace(/--.*$/gm, '').trim();
// Session and transaction control: runs on a read-only connection and changes no data.
const control = (p: string) => /^(set|reset|begin|start|commit|rollback|end|savepoint|release|pragma)\b/i.test(bare(p));
// Loose mirror of `check_read_only`: statements that would make a read-only session writable.
function escapesReadOnly(sql: string) {
  const p = bare(sql);
  if (/\b(default_transaction_read_only|transaction_read_only|tx_read_only|query_only)\b|\bread\s+write\b|^set\s+session\s+characteristics\b|^(reset|discard)\s+all\b/i.test(p)) return true;
  return /^set\s+(session\s+|global\s+)?transaction\b/i.test(p) && !/^set\s+(session\s+)?transaction\s+read\s+only$/i.test(p);
}

function dropSessionsOf(connId: string) {
  for (const [tab, s] of sessions) if (s.connId === connId) sessions.delete(tab);
}

const newId = (name: string) => name.trim().toLowerCase().replace(/[^a-z0-9]+/g, '-') + '-' + Date.now().toString(36);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function make(id: string, i: ConnectionInput, version: string, topology: Topology | null): Connection {
  const sqlite = i.engine === 'sqlite';
  const mongo = i.engine === 'mongo';
  const c: Connection = {
    id,
    name: i.name.trim(),
    engine: i.engine,
    version,
    env: i.env,
    host: sqlite ? '' : i.host.trim(),
    port: sqlite ? 0 : i.port || (mongo ? 27017 : 0),
    database: sqlite ? 'main' : i.database.trim(),
    user: sqlite ? '' : i.user.trim(),
    ssl: i.ssl,
    file: sqlite ? i.file.trim() : '',
    aiWrite: i.aiWrite,
    readOnly: i.readOnly,
    uri: '',
    authSource: mongo ? (i.authSource ?? '').trim() : '',
    topology: mongo ? topology : null,
  };
  if (!mongo || !i.uri?.trim()) return c;
  // As the backend does: the string decides, the fields only show what it says, and the password is not kept.
  const p = parseMongoUri(i.uri);
  if (!p) throw new ApiError({ code: 'invalid_input', message: 'Invalid connection string', detail: 'It must start with mongodb:// or mongodb+srv://' });
  return { ...c, uri: p.redacted, host: p.host, port: p.port, user: p.user, database: p.database, authSource: p.authSource, ssl: p.ssl };
}

// Same mapping as the prototype: each engine shows one of the mock schemas.
const mockSchemaOf: Record<Engine, string> = { pg: 'prod', mysql: 'shop', sqlite: 'notes', mongo: '' };
const mockSchema = (c: Connection) => schemas[mockSchemaOf[c.engine]];

function find(id: string) {
  const c = list.find((x) => x.id === id);
  if (!c) throw new ApiError({ code: 'not_found', message: `Connection ${id} not found` });
  return c;
}

/** `find` for the SQL paths (browse, query, edit, AI): MongoDB fails with `unsupported`, like the backend. */
function findSql(id: string) {
  const c = find(id);
  if (c.engine === 'mongo') throw new ApiError({ code: 'unsupported', message: 'This feature is not available for MongoDB connections yet.' });
  return c;
}

// The two databases every mock MongoDB connection browses, like the seeded test stack.
const mongoCollections: Record<string, TableInfo[]> = {
  shop_staging: [
    { name: 'customers', kind: 'table', rowEstimate: 420 },
    { name: 'orders', kind: 'table', rowEstimate: 5160 },
    { name: 'products', kind: 'table', rowEstimate: 87 },
    { name: 'order_summary', kind: 'view' },
  ],
  analytics: [
    { name: 'events', kind: 'table', rowEstimate: 10234 },
    { name: 'metrics', kind: 'table', rowEstimate: 1 },
  ],
};

// Field paths of one collection, as the backend's document sample infers them. `sampleSize` is the
// same on every row; `frequency` is the share of sampled documents that have the field.
function mongoFields(sampleSize: number, fields: [name: string, dataType: string, frequency?: number][]): ColumnInfo[] {
  return fields.map(([name, dataType, frequency = 1]) => ({ name, dataType, nullable: frequency < 1, isPk: name === '_id', pkOrdinal: name === '_id' ? 1 : undefined, frequency, sampleSize }));
}

const mongoSchema: Record<string, ColumnInfo[]> = {
  customers: mongoFields(1000, [
    ['_id', 'objectId'],
    ['name', 'string'],
    ['email', 'string'],
    ['country', 'string'],
    ['plan', 'string'],
    ['address.city', 'string', 0.87],
    ['address.zip', 'string', 0.62],
    ['created_at', 'date'],
  ]),
  orders: mongoFields(1000, [
    ['_id', 'objectId'],
    ['customer_id', 'objectId'],
    ['status', 'string'],
    ['total', 'decimal128'],
    ['paid', 'bool'],
    ['items[].sku', 'string', 0.94],
    ['items[].qty', 'int32', 0.94],
    ['items[].price', 'decimal128', 0.94],
    ['shipped_at', 'date', 0.71],
  ]),
  products: mongoFields(1000, [
    ['_id', 'objectId'],
    ['sku', 'string'],
    ['name', 'string'],
    ['price', 'decimal128 | int32 | string', 0.98],
    ['stock', 'int32', 0.91],
    ['tags', 'array', 0.45],
  ]),
  order_summary: mongoFields(48, [
    ['_id', 'objectId'],
    ['customer', 'string'],
    ['orders', 'int32'],
    ['revenue', 'decimal128'],
    ['last_order', 'date'],
  ]),
  events: mongoFields(1000, [
    ['_id', 'objectId'],
    ['user_id', 'int64'],
    ['name', 'string'],
    ['source', 'string'],
    ['meta.ua', 'string', 0.83],
    ['created_at', 'date'],
  ]),
  metrics: mongoFields(1, [
    ['_id', 'objectId'],
    ['name', 'string'],
    ['value', 'double'],
  ]),
};

// ---- MongoDB structure mock: plausible indexes, validator, options and stats ------------------

/** An index the mock reports, before it becomes the `IndexInfo` the grid and Properties show. */
type MockIndex = { name: string; keys: Record<string, 1 | -1 | string>; unique?: boolean; sparse?: boolean; hidden?: boolean; ttl?: number };

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

/** Options, validator and indexes of each seed collection; the fields above give the rest. */
const mockMongoSpecs: Record<string, { options?: Record<string, unknown>; validator?: Record<string, unknown>; level?: string; action?: string; indexes?: MockIndex[] }> = {
  customers: {
    indexes: [{ name: '_id_', keys: { _id: 1 } }, { name: 'email_1', keys: { email: 1 }, unique: true }],
  },
  orders: {
    options: { collation: { locale: 'en', strength: 2 } },
    validator: {
      $jsonSchema: {
        bsonType: 'object',
        required: ['status', 'total'],
        properties: {
          status: { enum: ['paid', 'shipped', 'pending', 'cancelled', 'refunded'] },
          total: { bsonType: ['decimal', 'int'] },
        },
      },
    },
    level: 'strict',
    action: 'error',
    indexes: [
      { name: '_id_', keys: { _id: 1 } },
      { name: 'status_1_created_at_-1', keys: { status: 1, created_at: -1 } },
      { name: 'customer_id_1', keys: { customer_id: 1 } },
      { name: 'items_sku_1', keys: { 'items.sku': 1 }, sparse: true },
    ],
  },
  products: {
    indexes: [{ name: '_id_', keys: { _id: 1 } }, { name: 'sku_1', keys: { sku: 1 }, unique: true }, { name: 'name_text', keys: { name: 'text' }, hidden: true }],
  },
  events: {
    indexes: [{ name: '_id_', keys: { _id: 1 } }, { name: 'created_at_1', keys: { created_at: 1 }, ttl: 2592000 }],
  },
  metrics: {
    options: { capped: true, size: 1048576, max: 1000 },
    indexes: [{ name: '_id_', keys: { _id: 1 } }, { name: 'name_1', keys: { name: 1, _id: -1 }, unique: true }],
  },
  order_summary: {
    options: {
      viewOn: 'orders',
      pipeline: [{ $match: { status: 'paid' } }, { $group: { _id: '$customer_id', orders: { $sum: 1 }, revenue: { $sum: '$total' } } }],
    },
  },
};

/** The seed collection and its spec; an unknown name fails like the backend's `not_found`. */
function mockMongoSpec(database: string, coll: string) {
  const info = (mongoCollections[database] ?? []).find((t) => t.name === coll);
  if (!info) throw new ApiError({ code: 'not_found', message: `Collection ${coll} not found` });
  return { info, spec: mockMongoSpecs[coll] ?? {} };
}

const prettyJson = (o: unknown) => JSON.stringify(o, null, 2);

/** Collection kind, validator, options and stats, as the backend sends them for MongoDB. */
function mockMongoProperties(database: string, coll: string): TableProperties {
  const { info, spec } = mockMongoSpec(database, coll);
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

/** The mongosh script that creates the collection with its options, validator and indexes. */
function mockMongoScript(database: string, coll: string): string {
  const { info, spec } = mockMongoSpec(database, coll);
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

// ---- MongoDB mock: find() over deterministic sample documents --------------------------------

/** Methods the Rust guard classifies as reads; the mock runs `find` of these only. */
const MONGO_READS = /^(find|findOne|aggregate|countDocuments|estimatedDocumentCount|distinct|getIndexes|stats)$/;

/** Flattened column of a field path: array element paths collapse to the array field itself. */
const mongoFlatColumn = (path: string) => path.replace(/\[\].*$/, '');

// Pool values for the field names the seed collections use; other strings derive from the field name.
const mongoWords: Record<string, string[]> = {
  name: ['Livia Saris', 'Jaydon Aminoff', 'Maria Lubin', 'Ann Press', 'John Clarkson', 'Aspen Lubin', 'Kierra Gouse', 'Omar Dias'],
  customer: ['Livia Saris', 'Jaydon Aminoff', 'Maria Lubin', 'Ann Press'],
  email: ['livia@example.com', 'jaydon@example.com', 'maria@example.com', 'ann@example.com'],
  city: ['Berlin', 'Jakarta', 'Lisbon', 'Osaka', 'Toronto'],
  country: ['ID', 'US', 'DE', 'SG', 'JP', 'NL'],
  plan: ['free', 'pro', 'team'],
  status: ['paid', 'shipped', 'pending', 'cancelled', 'refunded'],
  sku: ['SKU-1001', 'SKU-1002', 'SKU-1003', 'SKU-1004'],
  ua: ['Mozilla/5.0 (Macintosh)', 'Mozilla/5.0 (Windows NT 10.0)', 'curl/8.4.0'],
  source: ['web', 'ios', 'android', 'api'],
};

/** One sample value in relaxed Extended JSON, deterministic per field path, row and array element. */
function mongoSample(path: string, t: string, i: number, e: number): unknown {
  const leaf = path.split('.').at(-1)!.replace('[]', '');
  switch (t) {
    case 'objectId': return { $oid: '64a7f2b8c9d0e1f2a3b4' + ((i * 7 + e + path.length * 11) % 65536).toString(16).padStart(4, '0') };
    case 'date': return { $date: new Date(Date.UTC(2026, 0, 2 + i, 9 + e, 30)).toISOString() };
    case 'decimal128': return { $numberDecimal: (5 + ((i * 13 + e * 7) % 400) / 4).toFixed(2) };
    // int64 values past 2^53 keep their digits in $numberLong, as the backend sends them.
    case 'int64': return { $numberLong: String(9007199254740993 + i * 1000 + e) };
    case 'int32': return (i * 7 + e * 3) % 100;
    case 'double': return Math.round((i + e) * 3.14 * 100) / 100;
    case 'bool': return (i + e) % 2 === 0;
    case 'array': return ['new', 'sale', 'hot'].slice(0, (i % 3) + 1);
    default: {
      const pool = mongoWords[leaf];
      if (pool) return pool[(i + e) % pool.length];
      if (leaf === 'zip') return String(10100 + i * 7 + e);
      return `${leaf} ${i + e}`;
    }
  }
}

/** Sets a dotted path; a `[]` segment becomes an array of two element documents. */
function mongoSetPath(doc: Record<string, unknown>, segs: string[], value: (e: number) => unknown) {
  const head = segs[0];
  if (head.endsWith('[]')) {
    const arr = (doc[head.slice(0, -2)] ??= []) as Record<string, unknown>[];
    for (let e = 0; e < 2; e++) {
      arr[e] ??= {};
      mongoSetPath(arr[e], segs.slice(1), () => value(e));
    }
    return;
  }
  if (segs.length === 1) {
    doc[head] = value(0);
    return;
  }
  const next = (doc[head] ??= {}) as Record<string, unknown>;
  mongoSetPath(next, segs.slice(1), value);
}

/** Sample documents of one collection: the same query always shows the same rows. */
function mongoDocs(coll: string): Record<string, unknown>[] {
  const fields = mongoSchema[coll];
  if (!fields) return [];
  const docs: Record<string, unknown>[] = [];
  for (let i = 0; i < 3 + (coll.length % 6); i++) {
    const doc: Record<string, unknown> = {};
    for (const f of fields) {
      // frequency < 1: some sampled documents lack the field, as the tree's percentage says.
      if (f.frequency !== undefined && (i * 37 + f.name.length * 13) % 100 >= f.frequency * 100) continue;
      mongoSetPath(doc, f.name.split('.'), (e) => mongoSample(f.name, f.dataType.split('|')[0].trim(), i, e));
    }
    docs.push(doc);
  }
  return docs;
}

/** Value at a dotted path of a document, or undefined when it has no such field (projection and sort). */
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

/** Grid cell of one relaxed Extended JSON value, with its BSON type name. */
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

/** A filter value the mock compares with the sample documents. */
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

/** A quoted JavaScript string, with the escapes the sample text uses. */
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

/** One value of the find filter: a literal, an array or a document. */
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

/** A document value as the mock compares it with the sample documents. */
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
  return (a as { v?: unknown }).v !== undefined && (a as { v?: unknown }).v === (b as { v?: unknown }).v;
}

/** Order of two values of the same kind, for $gt and sort; null when they cannot be ordered. */
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

/** `$and`, `$or`, field conditions and a literal document; an unknown `$` operator fails with its name. */
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

/** One `find()` call: the collection, its filter, projection and cursor chain. */
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

/** The mock's MongoDB run: a find() with its filter, projection and cursor chain over the sample documents. */
function mongoRun(c: Connection, sql: string, limit: number, sort?: ResultSort) {
  const parts = sql.split(';').map((p) => p.trim()).filter((p) => p.replace(/\/\/.*$/gm, '').trim());
  if (!parts.length) throw new ApiError({ code: 'invalid_input', message: 'Nothing to run' });
  const started = Date.now();
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
      isRead: true,
      sortable: true,
      elapsedMs: 1,
      documents: docs,
      cellTypes: cells.map((row) => row.map((x) => x.t)),
    };
  });
  return { statements, elapsedMs: Date.now() - started };
}

/** Method and collection of a `db.<coll>.<method>(...)` statement, for the Mongo guard stand-ins. */
function mongoMethod(part: string): { coll?: string; method: string } | null {
  const m = /^db\s*\.\s*(?:getCollection\(\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')\s*\)|([A-Za-z_$][\w$]*))\s*\.\s*([A-Za-z_$][\w$]*)/.exec(part.trim());
  if (!m) return null;
  return { coll: m[2] ?? m[1].slice(1, -1).replace(/\\(["'\\])/g, '$1'), method: m[3] };
}

function mockGuard(sql: string): GuardCheck {
  const parts = sql.split(';').map((p) => p.trim()).filter((p) => statementKind(p) !== 'empty');
  if (parts.length !== 1) return { allowed: false, reason: parts.length ? `Exactly one statement is allowed, found ${parts.length}` : 'The query is empty' };
  const kind = parts[0].split(/\s+/)[0].toUpperCase();
  if (statementKind(parts[0]) !== 'read') return { allowed: false, reason: `${kind} is not a read (mock guard)`, kind };
  if (/\b(analy[sz]e|into|for\s+update|for\s+share)\b/i.test(parts[0])) return { allowed: false, reason: 'Not a plain read (mock guard)', kind };
  return { allowed: true, kind: kind === 'WITH' ? 'SELECT' : kind };
}

function mockClassifyWrite(sql: string): WriteInfo {
  const parts = sql.split(';').map((p) => p.trim()).filter((p) => statementKind(p) !== 'empty');
  const info: WriteInfo = { statements: [], warnings: [] };
  if (parts.length > 1) info.warnings.push({ code: 'multiple_statements', message: `${parts.length} statements will run` });
  parts.forEach((p, i) => {
    const kind = p.split(/\s+/)[0].toUpperCase();
    const table = p.match(/(?:update|from|into|table)\s+([\w.]+)/i)?.[1];
    info.statements.push({ kind, table });
    const where = /\bwhere\b/i.test(p);
    if (kind === 'UPDATE' && !where) info.warnings.push({ code: 'update_without_where', message: `UPDATE without WHERE changes every row of ${table ?? 'the table'}`, statement: i });
    if (kind === 'DELETE' && !where) info.warnings.push({ code: 'delete_without_where', message: `DELETE without WHERE removes every row of ${table ?? 'the table'}`, statement: i });
    if (kind === 'DROP') info.warnings.push({ code: 'drop', message: `DROP ${table ?? ''} cannot be undone`, statement: i });
    if (kind === 'TRUNCATE') info.warnings.push({ code: 'truncate', message: `TRUNCATE removes every row of ${table ?? 'the table'}`, statement: i });
  });
  return info;
}

/** Keyword stand-in of the Rust Mongo guard: one statement, and only a read method passes. */
function mongoGuard(sql: string): GuardCheck {
  const parts = sql.split(';').map((p) => p.trim()).filter((p) => p.replace(/\/\/.*$/gm, '').trim());
  if (parts.length !== 1) return { allowed: false, reason: parts.length ? `Exactly one statement is allowed, found ${parts.length}` : 'The query is empty' };
  const m = mongoMethod(parts[0]);
  if (!m) return { allowed: false, reason: 'Not a collection read (mock guard)', kind: parts[0].split(/\s+/)[0] };
  if (!MONGO_READS.test(m.method)) return { allowed: false, reason: `${m.method} is not a read (mock guard)`, kind: m.method };
  return { allowed: true, kind: m.method };
}

/** Keyword stand-in of the Rust Mongo classify_write: the method names by regex. */
function mongoClassifyWrite(sql: string, database?: string | null): WriteInfo {
  const parts = sql.split(';').map((p) => p.trim()).filter((p) => p.replace(/\/\/.*$/gm, '').trim());
  const info: WriteInfo = { statements: [], warnings: [] };
  if (parts.length > 1) info.warnings.push({ code: 'multiple_statements', message: `${parts.length} statements will run` });
  let db = database ?? '';
  parts.forEach((p, i) => {
    // `use` changes the database for the statements after it, as the run does.
    const use = /^use\s+([^\s;]+)/.exec(p);
    if (use) {
      db = use[1];
      info.statements.push({ kind: 'use', database: db || undefined, read: true });
      return;
    }
    // A db-level method has no collection part for `mongoMethod`.
    if (/^db\s*\.\s*dropDatabase\s*\(/.test(p)) {
      info.statements.push({ kind: 'dropDatabase', database: db || undefined, read: false });
      info.warnings.push({ code: 'drop', message: 'dropDatabase drops the current database and cannot be undone', statement: i });
      return;
    }
    const m = mongoMethod(p);
    const kind = m?.method ?? 'unknown';
    info.statements.push({ kind, table: m?.coll, database: db || undefined, read: !!m && MONGO_READS.test(m.method) });
    if (!m) return;
    // An empty filter document changes or removes every document of the collection.
    const args = p.slice(p.indexOf('(', p.indexOf(kind)) + 1);
    if ((kind === 'updateMany' || kind === 'deleteMany') && /^\s*\{\s*\}\s*[,)]/.test(args)) {
      info.warnings.push({
        code: kind === 'updateMany' ? 'update_without_where' : 'delete_without_where',
        message: `${kind} with an empty filter changes every document of ${m.coll ?? 'the collection'}`,
        statement: i,
      });
    }
    if (kind === 'drop') info.warnings.push({ code: 'drop', message: `drop removes ${m.coll ?? 'the collection'} and cannot be undone`, statement: i });
  });
  return info;
}

// Foreign keys guessed from names: `<x>_id` points at `<x>s.id` or `<x>.id` in the same mock schema (and PG namespace).
function mockForeignKeys(c: Connection): ForeignKeyInfo[] {
  const s = mockSchema(c);
  const pg = c.engine === 'pg';
  const fks: ForeignKeyInfo[] = [];
  for (const [table, cols] of Object.entries(s)) {
    for (const col of cols) {
      const m = /^(.+)_id$/.exec(col);
      if (!m) continue;
      const ref = [`${m[1]}s`, m[1]].find((t) => s[t] && nsOf(c.engine, t) === nsOf(c.engine, table));
      if (!ref) continue;
      fks.push({
        name: `${table}_${col}_fkey`,
        schema: pg ? nsOf(c.engine, table) : undefined,
        table,
        columns: [col],
        refSchema: pg ? nsOf(c.engine, ref) : undefined,
        refTable: ref,
        refColumns: ['id'],
        onUpdate: 'NO ACTION',
        onDelete: 'NO ACTION',
      });
    }
  }
  return fks;
}

function mockProperties(c: Connection, table: string): TableProperties {
  const pk = pkOf(table);
  const fks = mockForeignKeys(c);
  const foreignKeys = fks.filter((f) => f.table === table);
  return {
    indexes: [
      { name: `${table}_pkey`, columns: pk, unique: true, primary: true, method: 'btree' },
      ...foreignKeys.map((f) => ({ name: `idx_${table}_${f.columns[0]}`, columns: f.columns, unique: false, primary: false, method: 'btree' })),
    ],
    constraints: [{ name: `${table}_pkey`, kind: 'primary', columns: pk }],
    foreignKeys,
    references: fks.filter((f) => f.refTable === table),
  };
}

function mockEditable(c: Connection, sql: string): EditableResult {
  const r = runMock(sql, mockSchema(c), c);
  if (r.error || !r.table) return { pk: [], reason: r.error ?? 'Run a SELECT to edit its rows' };
  const clean = sql.replace(/--.*$/gm, '');
  const table = r.table;
  const pk = pkOf(table);
  let reason: string | undefined;
  if (/^\s*with\b/i.test(clean) || /\bunion\b/i.test(clean)) reason = 'Read-only: result is not from a single table';
  else if (/\bjoin\b/i.test(clean) || /\bfrom\s+[\w.]+(\s+\w+)?\s*,/i.test(clean)) reason = 'Read-only: result joins several tables';
  else if (/\b(group\s+by|distinct)\b|\b(count|sum|avg|min|max)\s*\(/i.test(clean)) reason = 'Read-only: result has aggregates';
  const list = clean.match(/select\s+([\s\S]*?)\s+from\s/i)?.[1]?.trim() ?? '';
  if (!reason && list !== '*' && !/^\w+(\s*,\s*\w+)*$/.test(list)) reason = 'Read-only: result has computed columns';
  const cols = list === '*' ? r.columns : list.split(/\s*,\s*/);
  if (!reason && !pk.every((k) => cols.includes(k))) reason = `Read-only: primary key (${pk.join(', ')}) is not in the result`;
  return { table, schema: nsOf(c.engine, table), pk, reason };
}

// The demo build keeps the AI settings in memory and never calls a model.
let aiSettings: AiSettings = { provider: 'openai', baseUrl: 'http://localhost:11434/v1', model: 'llama3.1:8b', privacy: 'results', maxRows: 500, timeoutSec: 15 };
let aiKey = '';
const noAi = () => new ApiError({ code: 'invalid_input', message: 'The demo build has no AI backend. Use the desktop app or the dev bridge.' });

export const mockBackend: Backend = {
  kind: 'mock',
  async listConnections() {
    return list.map((c) => ({ ...c }));
  },
  async createConnection(input) {
    const c = make(newId(input.name), input, input.version || versions[input.engine], input.topology ?? (input.engine === 'mongo' ? 'replica_set' : null));
    list.push(c);
    return { ...c };
  },
  // Saving or deleting a connection drops its tab sessions, as the backend does.
  async updateConnection(id, input) {
    const cur = find(id);
    const c = make(id, input, input.version || cur.version, input.topology ?? cur.topology);
    list = list.map((x) => (x.id === id ? c : x));
    dropSessionsOf(id);
    return { ...c };
  },
  async duplicateConnection(id, input) {
    const src = find(id);
    const i: ConnectionInput = input ?? { ...src, name: `${src.name} (copy)`, aiWrite: false };
    const c = make(newId(i.name), i, src.version, i.topology ?? src.topology);
    list.push(c);
    return { ...c };
  },
  async deleteConnection(id) {
    find(id);
    list = list.filter((x) => x.id !== id);
    dropSessionsOf(id);
  },
  async reorderConnections(ids) {
    const next = ids.map((id) => list.find((c) => c.id === id));
    if (new Set(ids).size !== list.length || next.length !== list.length || next.some((c) => !c)) throw new ApiError({ code: 'invalid_input', message: 'The connection order is out of date' });
    list = next as Connection[];
    return list.map((c) => ({ ...c }));
  },
  async testConnection(input) {
    await sleep(900);
    if (input.engine === 'mongo' && input.uri?.trim() && !parseMongoUri(input.uri)) throw new ApiError({ code: 'invalid_input', message: 'Invalid connection string', detail: 'It must start with mongodb:// or mongodb+srv://' });
    return { serverVersion: versions[input.engine], latencyMs: 12, ...(input.engine === 'mongo' ? { topology: 'replica_set' as const } : {}) };
  },
  // SQL schema browsing and queries run on the prototype data in ../mock.ts; MongoDB browses `mongoCollections`.
  async listDatabases(connId) {
    const c = find(connId);
    if (c.engine === 'mongo') return ['analytics', 'shop_staging'];
    return [c.database];
  },
  async listSchemas(connId) {
    const c = find(connId);
    if (c.engine !== 'pg') return [];
    return [...new Set(Object.keys(mockSchema(c)).map((t) => nsOf(c.engine, t)))];
  },
  async listTables(connId, database, schema) {
    const c = find(connId);
    if (c.engine === 'mongo') return mongoCollections[database] ?? [];
    return Object.keys(mockSchema(c))
      .filter((t) => c.engine !== 'pg' || nsOf(c.engine, t) === (schema ?? 'public'))
      .map((name) => ({ name, kind: 'table' as const }));
  },
  async listColumns(connId, _database, _schema, table) {
    const c = find(connId);
    if (c.engine === 'mongo') {
      const cols = mongoSchema[table];
      if (!cols) throw new ApiError({ code: 'not_found', message: `Collection ${table} not found` });
      return cols;
    }
    const s = mockSchema(c);
    if (!s[table]) throw new ApiError({ code: 'not_found', message: `Table ${table} not found` });
    const pk = pkOf(table);
    return s[table].map((name) => ({ name, dataType: colType(s, table, name), nullable: !pk.includes(name), isPk: pk.includes(name), pkOrdinal: pk.includes(name) ? pk.indexOf(name) + 1 : undefined }));
  },
  async tableProperties(connId, database, _schema, table) {
    const c = find(connId);
    if (c.engine === 'mongo') return mockMongoProperties(database, table);
    const s = mockSchema(c);
    if (!s[table]) throw new ApiError({ code: 'not_found', message: `Table ${table} not found` });
    return mockProperties(c, table);
  },
  async schemaSnapshot(connId, database) {
    const c = find(connId);
    if (c.engine === 'mongo') {
      const tables = mongoCollections[database] ?? [];
      return { database, truncated: false, tables: tables.map((t) => ({ name: t.name, kind: t.kind, columns: (mongoSchema[t.name] ?? []).map((col) => ({ name: col.name, dataType: col.dataType })) })) };
    }
    const s = mockSchema(c);
    return {
      database,
      truncated: false,
      tables: Object.entries(s).map(([name, cols]) => ({ schema: c.engine === 'pg' ? nsOf(c.engine, name) : undefined, name, kind: 'table' as const, columns: cols.map((col) => ({ name: col, dataType: colType(s, name, col) })) })),
    };
  },
  async runQuery({ connId, sql, limit, tabId, sort }) {
    const c = find(connId);
    if (c.engine === 'mongo') return mongoRun(c, sql, limit, sort);
    const s = mockSchema(c);
    const parts = sql.split(';').map((p) => p.trim()).filter((p) => statementKind(p) !== 'empty');
    if (!parts.length) throw new ApiError({ code: 'invalid_input', message: 'Nothing to run' });
    // Refused before any statement runs, as the backend does: one write refuses the whole run.
    if (c.readOnly) {
      const refused = (m: string) => new ApiError({ code: 'connection_read_only', message: `Connection "${c.name}" is read-only: ${m}` });
      if (parts.some(escapesReadOnly)) throw refused("this statement would change the session's transaction mode");
      if (parts.some((p) => statementKind(p) === 'write' && !control(p))) throw refused('writes are refused');
    }
    // Session mode: the backend begins a transaction for the run and rolls a clean one back at the end, so a
    // tab keeps a session only while it holds changes (a typed COMMIT / ROLLBACK ends it as well).
    const manual = !!tabId && c.env === 'production' && !c.readOnly;
    let pending = manual && sessions.get(tabId)?.connId === connId;
    const statements: StatementResult[] = [];
    let total = 0;
    try {
      for (const [i, part] of parts.entries()) {
        const r = runMock(part, s, c, { limit, sort });
        if (r.error) throw new ApiError({ code: 'database', message: parts.length > 1 ? `Statement ${i + 1} of ${parts.length}: ${r.error}` : r.error });
        if (/^(commit|rollback|end)\b/i.test(bare(part))) pending = false;
        else if (statementKind(part) === 'write' && !control(part)) pending = true;
        total += r.ms;
        const rows = r.rows.slice(0, limit);
        statements.push({
          sql: part,
          columns: r.columns.map((name) => ({ name, dataType: r.table ? colType(s, r.table, name) : 'text' })),
          rows: rows.map((row) => r.columns.map((col) => row[col] as CellValue)),
          rowCount: rows.length,
          truncated: r.rows.length > rows.length,
          affected: r.columns.length ? undefined : 0,
          isRead: statementKind(part) === 'read',
          sortable: statementKind(part) === 'read' && /^(select|with|values)\b/i.test(bare(part)),
          elapsedMs: r.ms,
          table: r.table,
        });
      }
    } finally {
      if (manual) {
        if (pending) sessions.set(tabId, { connId });
        else sessions.delete(tabId);
      }
    }
    return { statements, elapsedMs: total };
  },
  async editableResult({ connId, sql }) {
    return mockEditable(findSql(connId), sql);
  },
  async applyEdits({ connId, sql, tabId, operations }) {
    const c = findSql(connId);
    if (c.readOnly) throw new ApiError({ code: 'connection_read_only', message: `Connection "${c.name}" is read-only: writes are refused` });
    const meta = mockEditable(c, sql);
    if (meta.reason || !meta.table) throw new ApiError({ code: 'invalid_input', message: meta.reason ?? 'Result is not editable' });
    const d = tableData(mockSchema(c), meta.table);
    const base = d.base.map((r) => ({ ...r }));
    const added = d.added.map((r) => ({ ...r }));
    const lists = [base, added];
    const findRow = (pk: Record<string, CellValue>) => {
      for (const rows of lists) {
        const index = rows.findIndex((r) => meta.pk.every((k) => r[k] === pk[k]));
        if (index >= 0) return { rows, index };
      }
      return null;
    };
    for (const [index, op] of operations.entries()) {
      if (op.kind !== 'delete' && meta.pk.some((k) => k in op.set && (op.set[k] === null || op.set[k] === ''))) {
        return { failed: { index, message: 'A primary-key column cannot be NULL. Transaction rolled back, nothing was saved.' } };
      }
      if (op.kind === 'insert') {
        if (meta.pk.some((k) => op.set[k] !== undefined) && findRow(Object.fromEntries(meta.pk.map((k) => [k, op.set[k] ?? null])))) {
          return { failed: { index, message: 'Duplicate primary key. Transaction rolled back, nothing was saved.' } };
        }
        added.push({ ...op.set });
        continue;
      }
      const found = findRow(op.pk);
      if (!found) return { failed: { index, message: 'The row no longer exists. Transaction rolled back, nothing was saved.' } };
      if (op.kind === 'delete') found.rows.splice(found.index, 1);
      else found.rows[found.index] = { ...found.rows[found.index], ...op.set };
    }
    d.base = base;
    d.added = added;
    if (tabId && c.env === 'production') sessions.set(tabId, { connId });
    return {};
  },
  // Mock values are never cut off, so the stored value is already the full one.
  async cellValue({ connId, sql, pk, column }) {
    const meta = mockEditable(findSql(connId), sql);
    if (meta.reason || !meta.table) throw new ApiError({ code: 'invalid_input', message: meta.reason ?? 'Result is not editable' });
    const d = tableData(mockSchema(find(connId)), meta.table);
    const row = [...d.base, ...d.added].find((r) => meta.pk.every((k) => r[k] === pk[k]));
    if (!row) throw new ApiError({ code: 'not_found', message: 'The row no longer exists or its primary key changed' });
    const v = row[column];
    return v === null || v === undefined ? null : typeof v === 'object' ? JSON.stringify(v) : String(v);
  },
  async cancelQuery() {},
  // Mock runs are instant; a run always finishes before progress could be polled.
  async queryProgress() {
    return null;
  },
  async commitTransaction(tabId) {
    if (!sessions.delete(tabId)) throw new ApiError({ code: 'invalid_input', message: 'No transaction is open in this tab' });
  },
  async rollbackTransaction(tabId) {
    if (!sessions.delete(tabId)) throw new ApiError({ code: 'invalid_input', message: 'No transaction is open in this tab' });
  },
  async tabStatus(tabId) {
    return sessions.has(tabId) ? { open: true, txOpen: true, pending: true, aborted: false } : null;
  },
  async closeTabSession(tabId) {
    sessions.delete(tabId);
  },
  async quitApp() {},
  async exportCsv(_connId, _database, _sql, _tabId) {
    throw new ApiError({ code: 'invalid_input', message: 'CSV export to a file needs the desktop app' });
  },
  async exportResultSql() {
    throw new ApiError({ code: 'invalid_input', message: 'SQL export to a file needs the desktop app' });
  },
  async exportDocuments() {
    throw new ApiError({ code: 'invalid_input', message: 'Document export to a file needs the desktop app' });
  },
  // The prototype data has types and primary keys only, so the DDL has no defaults or other constraints.
  async tableDdl(connId, database, schema, table) {
    const c = find(connId);
    if (c.engine === 'mongo') return mockMongoScript(database, table);
    const s = mockSchema(c);
    if (!s[table]) throw new ApiError({ code: 'not_found', message: `Table ${table} not found` });
    const pk = pkOf(table);
    const lines = s[table].map((col) => `  ${col} ${colType(s, table, col)}${pk.includes(col) ? ' NOT NULL' : ''}`);
    if (pk.length) lines.push(`  PRIMARY KEY (${pk.join(', ')})`);
    return `CREATE TABLE ${schema ? schema + '.' : ''}${table} (\n${lines.join(',\n')}\n);`;
  },
  async exportTableSql() {
    throw new ApiError({ code: 'invalid_input', message: 'SQL export to a file needs the desktop app' });
  },
  async savePdf() {
    throw new ApiError({ code: 'invalid_input', message: 'Saving a PDF to a file needs the desktop app' });
  },
  async guardCheck(engine, sql) {
    return engine === 'mongo' ? mongoGuard(sql) : mockGuard(sql);
  },
  async classifyWrite(engine, sql, database) {
    return engine === 'mongo' ? mongoClassifyWrite(sql, database) : mockClassifyWrite(sql);
  },
  async runAiRead(connId, database, sql) {
    const g = find(connId).engine === 'mongo' ? mongoGuard(sql) : mockGuard(sql);
    if (!g.allowed) throw new ApiError({ code: 'guard_rejected', message: `Blocked by the read-only guard: ${g.reason}` });
    const run = await mockBackend.runQuery({ connId, database: database ?? '', sql, queryId: 'ai', limit: 500 });
    const s = run.statements[0];
    return { columns: s.columns, rows: s.rows, truncated: s.truncated, elapsedMs: s.elapsedMs, ...(s.documents ? { documents: s.documents } : {}) };
  },
  async getAiSettings() {
    return { ...aiSettings, hasKey: !!aiKey };
  },
  async saveAiSettings(settings) {
    aiSettings = { ...settings };
    return { ...aiSettings, hasKey: !!aiKey };
  },
  async setAiKey(key) {
    aiKey = key ?? '';
  },
  async testAi() {
    throw noAi();
  },
  async listAiModels() {
    throw noAi();
  },
  async chatSend() {
    throw noAi();
  },
  async chatCancel() {},
  async approveWrite() {
    throw noAi();
  },
  async denyWrite() {
    throw noAi();
  },
};
