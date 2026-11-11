// sandb test seed (MongoDB 8): deterministic data. Runs in mongosh during container init.
// Same business data as the SQL seed where possible, so AI tests can compare engines.
// Shapes on purpose: nested documents, arrays, arrays of documents, missing fields,
// every BSON type the app must keep, a mixed-type field, a view, a time series
// collection and a collection of large (~2 MB) documents.

const NAMES = ['Livia Saris', 'Jaydon Aminoff', 'Maria Lubin', 'Ann Press', 'John Clarkson',
  'Aspen Lubin', 'Kierra Gouse', 'Omar Dias', 'Mila Novak', 'Theo Grant'];
const COUNTRIES = ['ID', 'US', 'DE', 'SG', 'JP', 'NL'];
const CITIES = { ID: 'Jakarta', US: 'Austin', DE: 'Berlin', SG: 'Singapore', JP: 'Osaka', NL: 'Utrecht' };
const PLANS = ['free', 'pro', 'team'];
const STATUSES = ['paid', 'shipped', 'pending', 'cancelled', 'refunded'];
const PRODUCT_KINDS = ['Desk', 'Lamp', 'Chair', 'Monitor', 'Keyboard'];
const EVENT_NAMES = ['page_view', 'signup', 'login', 'add_to_cart', 'checkout'];
const SOURCES = ['web', 'ios', 'android'];
const DAY = 24 * 60 * 60 * 1000;

function insertInBatches(coll, count, make, batch = 1000) {
  for (let start = 1; start <= count; start += batch) {
    const docs = [];
    for (let i = start; i < start + batch && i <= count; i++) docs.push(make(i));
    coll.insertMany(docs, { ordered: true });
  }
}

function money(cents) {
  return NumberDecimal((cents / 100).toFixed(2));
}

// ---------- shop_staging ----------
const shop = db.getSiblingDB('shop_staging');

insertInBatches(shop.customers, 500, (i) => {
  const name = NAMES[i % 10];
  const country = COUNTRIES[i % 6];
  const doc = {
    _id: i,
    name,
    email: `${name.toLowerCase().replace(' ', '.')}.${i}@mail.com`,
    plan: PLANS[i % 3],
    created_at: new Date(Date.UTC(2026, 0, 1) + (i % 300) * DAY),
    address: {
      city: CITIES[country],
      country,
      geo: { lat: -6 + (i % 90) / 10, lng: 106 + (i % 70) / 10 },
    },
    tags: i % 4 === 0 ? [] : ['newsletter', 'beta', 'vip'].slice(0, i % 4),
  };
  // Missing field: only every third customer has a phone.
  if (i % 3 === 0) doc.phone = `+62-811-${String(1000 + i).padStart(4, '0')}`;
  return doc;
});

insertInBatches(shop.products, 500, (i) => {
  const doc = {
    _id: i,
    sku: `SKU-${1000 + i}`,
    name: `${PRODUCT_KINDS[i % 5]} ${i}`,
    // Mixed types on purpose: Decimal128 normally, Int32 every 25th, String every 40th.
    price: i % 40 === 0 ? String(5 + (i * 37 % 40000) / 100)
      : i % 25 === 0 ? NumberInt(5 + Math.floor((i * 37 % 40000) / 100))
        : money(500 + (i * 37 % 40000)),
  };
  // Missing field: stock is absent where the SQL seed has NULL.
  if (i % 7 !== 0) doc.stock = NumberInt(i * 13 % 300);
  return doc;
});

insertInBatches(shop.orders, 500, (i) => {
  const items = [];
  for (let k = 0; k <= i % 3; k++) {
    items.push({
      product_id: 1 + ((i + k) * 11) % 500,
      qty: NumberInt(1 + (i + k) % 5),
      price: money(500 + ((i + k) * 211 % 20000)),
    });
  }
  const doc = {
    _id: i,
    customer_id: 1 + (i * 7) % 500,
    status: STATUSES[i % 5],
    total: money(500 + (i * 7919 % 90000)),
    paid: i % 10 >= 3,
    items,
    history: [
      { status: 'pending', at: new Date(Date.UTC(2026, 9, 1) + i * 120000) },
      { status: 'paid', at: new Date(Date.UTC(2026, 9, 1) + i * 120000 + 60000) },
    ],
  };
  if (i % 5 === 1) doc.shipped_at = new Date(Date.UTC(2026, 9, 1) + (i % 30) * DAY);
  return doc;
});

// One document for each BSON type that the app must keep, plus a document with nulls.
shop.type_zoo.insertMany([
  {
    _id: ObjectId('65f000000000000000000001'),
    c_int32: NumberInt(42),
    c_int64: NumberLong('9007199254740993'),
    c_double: 3.14159,
    c_decimal: NumberDecimal('12345.6789'),
    c_string: 'héllo wörld ✓',
    c_bool: true,
    c_null: null,
    c_date: ISODate('2026-10-01T09:30:00.123Z'),
    c_timestamp: Timestamp({ t: 1790000000, i: 7 }),
    c_binary: BinData(0, 'AAECAwQFBgcICQ=='),
    c_uuid: UUID('3b241101-e2bb-4255-8caf-4136c566a962'),
    c_regex: /^sandb-[a-z]+$/i,
    c_minkey: MinKey(),
    c_maxkey: MaxKey(),
    c_object_id: ObjectId('65f0000000000000000000ff'),
    c_document: { a: NumberInt(1), b: { c: 'deep' } },
    c_array: [NumberInt(1), 'two', 3.0, { four: 4 }, [5]],
  },
  {
    _id: ObjectId('65f000000000000000000002'),
    c_int32: null,
    c_int64: NumberLong(-1),
    c_double: -0.5,
    c_decimal: NumberDecimal('-0.0001'),
    c_string: '',
    c_bool: false,
    c_date: ISODate('1969-12-31T23:59:59.999Z'),
    c_array: [],
    c_document: {},
  },
]);

// A view, as the SQL seed has order_summary.
shop.createView('order_summary', 'orders', [
  { $lookup: { from: 'customers', localField: 'customer_id', foreignField: '_id', as: 'c' } },
  { $unwind: '$c' },
  { $project: { customer: '$c.name', status: 1, total: 1, paid: 1, shipped_at: 1 } },
]);

// Large documents (~2 MB each) for the memory and byte-limit tests.
const blob = 'x'.repeat(1024 * 1024);
for (let start = 1; start <= 100; start += 5) {
  const docs = [];
  for (let i = start; i < start + 5; i++) {
    docs.push({ _id: i, label: `big-${i}`, part1: blob, part2: blob });
  }
  shop.big_docs.insertMany(docs);
}

// ---------- analytics ----------
const analytics = db.getSiblingDB('analytics');

insertInBatches(analytics.daily_revenue, 120, (i) => ({
  _id: i,
  day: new Date(Date.UTC(2026, 5, 1) + (i - 1) * DAY),
  orders: NumberInt(20 + i * 13 % 180),
  revenue: money(500 + (i * 104729 % 2500000)),
  refunds: money(i * 811 % 90000),
}));

insertInBatches(analytics.events, 10000, (i) => ({
  _id: i,
  user_id: 1 + i % 500,
  name: EVENT_NAMES[i % 5],
  source: SOURCES[i % 3],
  created_at: new Date(Date.UTC(2026, 9, 1) + i * 60000),
  ...(i % 4 === 0 ? { props: { path: `/p/${i % 50}`, ms: NumberInt(i % 900) } } : {}),
}));

// Time series collection.
analytics.createCollection('metrics', {
  timeseries: { timeField: 'ts', metaField: 'host', granularity: 'minutes' },
});
insertInBatches(analytics.metrics, 1440, (i) => ({
  ts: new Date(Date.UTC(2026, 9, 1) + i * 60000),
  host: { name: `app-${i % 3}`, region: i % 2 === 0 ? 'ap-southeast-1' : 'eu-west-1' },
  cpu: (i * 7 % 100) / 100,
  mem_mb: NumberInt(512 + i % 512),
}));
