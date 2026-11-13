import type { ColumnInfo, Connection, Engine, TableInfo } from '../generated/wire';
import type { Row, Schema } from '../types';

export const versions: Record<Engine, string> = { pg: 'PostgreSQL 16.4', mysql: 'MySQL 8.4', sqlite: 'SQLite 3.46', mongo: 'MongoDB 8.0.3' };

export const seedConnections: Connection[] = [
  { id: 'prod', name: 'prod-analytics', engine: 'pg', version: 'PostgreSQL 16.4', env: 'production', host: 'prod-analytics.db.internal', port: 5432, database: 'analytics_prod', user: 'analyst', ssl: 'require', file: '', aiWrite: false, readOnly: false, uri: '', authSource: '', topology: null },
  { id: 'shop', name: 'shop-staging', engine: 'mysql', version: 'MySQL 8.4', env: 'staging', host: 'shop-staging.db.internal', port: 3306, database: 'shop', user: 'app', ssl: 'prefer', file: '', aiWrite: false, readOnly: false, uri: '', authSource: '', topology: null },
  { id: 'notes', name: 'notes.db', engine: 'sqlite', version: 'SQLite 3.46', env: 'local', host: '', port: 0, database: 'main', user: '', ssl: 'prefer', file: '~/data/notes.db', aiWrite: false, readOnly: false, uri: '', authSource: '', topology: null },
  { id: 'mongo', name: 'local mongo', engine: 'mongo', version: 'MongoDB 8.0.3', env: 'local', host: '127.0.0.1', port: 57017, database: 'shop_staging', user: 'sandb', ssl: 'disable', file: '', aiWrite: false, readOnly: false, uri: '', authSource: 'admin', topology: 'replica_set' },
];

export const schemas: Record<string, Schema> = {
  prod: {
    customers: ['id', 'name', 'email', 'country', 'plan', 'created_at'],
    orders: ['id', 'customer', 'status', 'total', 'paid', 'shipped_at'],
    order_items: ['id', 'order_id', 'product_id', 'qty', 'price'],
    products: ['id', 'sku', 'name', 'price', 'stock'],
    invoices: ['id', 'order_id', 'number', 'amount', 'issued_at'],
    daily_revenue: ['id', 'day', 'orders', 'revenue', 'refunds'],
    events: ['id', 'user_id', 'name', 'source', 'created_at'],
  },
  shop: {
    users: ['id', 'name', 'email', 'country', 'created_at'],
    carts: ['id', 'user_id', 'status', 'total', 'updated_at'],
    cart_items: ['id', 'cart_id', 'product_id', 'qty', 'price'],
    payments: ['id', 'cart_id', 'status', 'amount', 'paid_at'],
  },
  notes: {
    notes: ['id', 'title', 'body', 'pinned', 'created_at'],
    tags: ['id', 'name'],
    note_tags: ['note_id', 'tag_id'],
  },
};

// Tables outside the engine's default schema. Table names are unique across the mock data.
const tableNs: Record<string, string> = { daily_revenue: 'analytics', events: 'analytics' };
// MongoDB has no SQL namespace; no SQL view opens on it.
const defaultNs = { pg: 'public', mysql: 'shop', sqlite: 'main', mongo: '' } as const;
export const nsOf = (engine: keyof typeof defaultNs, table: string) => tableNs[table] ?? defaultNs[engine];

const mockSchemaOf: Record<Engine, string> = { pg: 'prod', mysql: 'shop', sqlite: 'notes', mongo: '' };
export const mockSchema = (c: Connection) => schemas[mockSchemaOf[c.engine]];

const names = ['Livia Saris', 'Jaydon Aminoff', 'Maria Lubin', 'Ann Press', 'John Clarkson', 'Aspen Lubin', 'Kierra Gouse', 'Omar Dias', 'Mila Novak', 'Theo Grant'];
const statuses = ['paid', 'shipped', 'pending', 'cancelled', 'refunded'];
const countries = ['ID', 'US', 'DE', 'SG', 'JP', 'NL'];
const plans = ['free', 'pro', 'team'];

function rng(seed: number) {
  return () => (seed = (seed * 16807) % 2147483647) / 2147483647;
}
const pick = <T>(r: () => number, a: T[]) => a[Math.floor(r() * a.length)];
const day = (r: () => number) => new Date(2026, 9, 1 + Math.floor(r() * 30)).toISOString().slice(0, 10);
const money = (r: () => number, max: number) => Math.round(r() * max * 100) / 100 + 5;

const gen: Record<string, (i: number, r: () => number) => Row> = {
  customers: (i, r) => {
    const n = pick(r, names);
    return { id: 501 + i, name: n, email: n.toLowerCase().replace(' ', '.') + '@mail.com', country: pick(r, countries), plan: pick(r, plans), created_at: day(r) };
  },
  orders: (i, r) => ({ id: 10234 + i, customer: pick(r, names), status: pick(r, statuses), total: money(r, 900), paid: r() > 0.3, shipped_at: r() > 0.4 ? day(r) : null }),
  order_items: (i, r) => ({ id: 1 + i, order_id: 10234 + Math.floor(r() * 120), product_id: 1 + Math.floor(r() * 40), qty: 1 + Math.floor(r() * 5), price: money(r, 200) }),
  products: (i, r) => ({ id: 1 + i, sku: 'SKU-' + (1000 + i), name: pick(r, ['Desk', 'Lamp', 'Chair', 'Monitor', 'Keyboard']) + ' ' + (i + 1), price: money(r, 400), stock: r() > 0.85 ? null : Math.floor(r() * 300) }),
  invoices: (i, r) => ({ id: 1 + i, order_id: 10234 + i, number: 'INV-2026-' + String(i + 1).padStart(4, '0'), amount: money(r, 900), issued_at: day(r) }),
  daily_revenue: (i, r) => ({ id: 1 + i, day: new Date(2026, 5, 1 + i).toISOString().slice(0, 10), orders: 20 + Math.floor(r() * 180), revenue: money(r, 25000), refunds: money(r, 900) }),
  events: (i, r) => ({ id: 90001 + i, user_id: 501 + Math.floor(r() * 120), name: pick(r, ['page_view', 'signup', 'login', 'add_to_cart', 'checkout']), source: pick(r, ['web', 'ios', 'android']), created_at: day(r) }),
};

const words = ['Weekly sync', 'Ideas', 'Groceries', 'Reading list', 'Bug notes', 'Trip plan', 'Draft post'];

function genericRow(cols: string[], i: number, r: () => number): Row {
  const row: Row = {};
  for (const c of cols) {
    if (c === 'id') row[c] = 1 + i;
    else if (c.endsWith('_id')) row[c] = 1 + Math.floor(r() * 120);
    else if (c === 'name') row[c] = pick(r, names);
    else if (c === 'email') row[c] = pick(r, names).toLowerCase().replace(' ', '.') + '@mail.com';
    else if (c === 'country') row[c] = pick(r, countries);
    else if (c === 'status') row[c] = pick(r, statuses);
    else if (c.endsWith('_at')) row[c] = r() > 0.15 ? day(r) : null;
    else if (['total', 'amount', 'price'].includes(c)) row[c] = money(r, 600);
    else if (c === 'qty') row[c] = 1 + Math.floor(r() * 5);
    else if (c === 'pinned') row[c] = r() > 0.7;
    else if (c === 'title') row[c] = pick(r, words);
    else if (c === 'body') row[c] = pick(r, ['Call Ann about Q4', 'Milk, eggs, coffee', 'Fix login timeout', 'Book train to Bandung']);
    else row[c] = pick(r, words);
  }
  return row;
}

// note_tags stays at 120: genericRow draws *_id values from 1..120, so more rows repeat its (note_id, tag_id) key.
export function rowsFor(table: string, cols: string[], n = table === 'note_tags' ? 120 : 500): Row[] {
  const r = rng(table.length * 7919);
  const g = gen[table];
  return Array.from({ length: n }, (_, i) => (g ? g(i, r) : genericRow(cols, i, r)));
}

const pks: Record<string, string[]> = { note_tags: ['note_id', 'tag_id'] };
export const pkOf = (table: string) => pks[table] ?? ['id'];

export function colType(schema: Schema, table: string, col: string) {
  const v = tableData(schema, table).base.find((r) => r[col] !== null)?.[col];
  if (typeof v === 'number') return Number.isInteger(v) ? 'int' : 'numeric';
  if (typeof v === 'boolean') return 'bool';
  if (typeof v === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(v)) return 'date';
  return 'text';
}

// Rows persist per schema so saved edits show up when the query runs again.
// Inserted rows live in `added` so they stay visible past the LIMIT slice.
const store = new WeakMap<Schema, Record<string, { base: Row[]; added: Row[] }>>();
export function tableData(schema: Schema, table: string) {
  let s = store.get(schema);
  if (!s) store.set(schema, (s = {}));
  return (s[table] ??= { base: rowsFor(table, schema[table]), added: [] });
}

/** The two databases every mock MongoDB connection browses, like the seeded test stack. */
export const mongoCollections: Record<string, TableInfo[]> = {
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

export const mongoSchema: Record<string, ColumnInfo[]> = {
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

/** An index the mock reports, before it becomes the `IndexInfo` the grid and Properties show. */
export type MockIndex = { name: string; keys: Record<string, 1 | -1 | string>; unique?: boolean; sparse?: boolean; hidden?: boolean; ttl?: number };

export const mongoSpecs: Record<string, { options?: Record<string, unknown>; validator?: Record<string, unknown>; level?: string; action?: string; indexes?: MockIndex[] }> = {
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
export function mongoDocs(coll: string): Record<string, unknown>[] {
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

/** Flattened column of a field path: array element paths collapse to the array field itself. */
export const mongoFlatColumn = (path: string) => path.replace(/\[\].*$/, '');
