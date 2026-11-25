import assert from 'node:assert/strict';
import { test } from 'node:test';
import { emptyMongoFilter, mongoFilterDocument, mongoQueryPath, validateMongoFilter } from '../src/lib/mongo/filter.ts';

const valid = [
  '{}',
  "{ status: 'paid' }",
  '{ total: { $gte: 10, $lt: 100 }, status: { $in: ["paid", "sent"] } }',
  '{ $and: [{ "items.sku": /abc/i }, { qty: { $ne: null } }] }',
  '{ _id: ObjectId("64b64c9d2d59c47ff5a80cf1"), created: { $lt: ISODate("2026-01-01") } }',
  '{ created: new Date("2026-01-01"), n: NumberLong("42"), d: NumberDecimal("1.25") }',
  '// typed in Compass\n{ status: "paid", /* trailing */ total: -1.5e2, }',
  '{ re: /^a+b\\/c$/i, empty: [], nested: { ok: true } }',
  '{ score: +Infinity, bad: NaN, missing: undefined }',
];

for (const doc of valid) {
  test(`accepts mongosh filter ${doc.slice(0, 42)}`, () => {
    assert.deepEqual(validateMongoFilter(doc), { valid: true });
  });
}

const invalid: [string, RegExp][] = [
  ['status: "paid"', /Expected '\{'/],
  ['{ status "paid" }', /Expected ':'/],
  ['{ status: "paid" ', /Unterminated document|Expected '\}'/],
  ['{ total: price + 1 }', /Unknown function: price|Expected '\)'/],
  ['{ total: Math.random() }', /Unexpected character/],
  ['{ created: Date() }', /Use new Date/],
  ['{ $where: function() { return true } }', /JavaScript is not supported: 'function'/],
  ['{ name: `abc${def}` }', /Template placeholders/],
  ['{ name: "unterminated }', /Unterminated string/],
  ['{ n: 1e }', /Invalid number/],
  ['{ n: 0x }', /Invalid number/],
];

for (const [doc, error] of invalid) {
  test(`rejects invalid filter ${doc.slice(0, 42)}`, () => {
    const result = validateMongoFilter(doc);
    assert.equal(result.valid, false);
    assert.match(result.valid ? '' : result.error, error);
  });
}

test('empty input and empty document are Compass empty filters', () => {
  assert.equal(emptyMongoFilter(''), true);
  assert.equal(emptyMongoFilter(' \n '), true);
  assert.equal(emptyMongoFilter('{}'), true);
  assert.equal(emptyMongoFilter('{ }'), false, 'spacing inside braces still counts as a typed document');
  assert.equal(mongoFilterDocument(''), '{}');
  assert.equal(mongoFilterDocument(" { status: 'paid' } "), "{ status: 'paid' }");
});

test('schema array paths complete as MongoDB query paths', () => {
  assert.equal(mongoQueryPath('items[].sku'), 'items.sku');
  assert.equal(mongoQueryPath('items[].variants[].sku'), 'items.variants.sku');
  assert.equal(mongoQueryPath('status'), 'status');
});
