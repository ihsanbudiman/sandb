import assert from 'node:assert/strict';
import { test } from 'node:test';
import { inDollarQuote } from '../src/lib/sql/text.ts';

// Completions must stay off inside a dollar-quoted `DO`/`AS $$…$$` body, including an unterminated
// one while the user is still typing at the end of the text (the cursor position `text.length`).
test('inDollarQuote covers an unterminated quote at end of text', () => {
  const open = 'do $$ begin select oi';
  assert.equal(inDollarQuote(open, open.length, 'pg'), true, 'cursor at end of an unterminated $$ body');
  assert.equal(inDollarQuote(open, 5, 'pg'), true);
  const closed = 'do $$ begin end $$; select x';
  assert.equal(inDollarQuote(closed, 10, 'pg'), true, 'inside a closed $$ body');
  assert.equal(inDollarQuote(closed, closed.length, 'pg'), false, 'after the closing $$');
  assert.equal(inDollarQuote('do $f$ a $f$ select', 7, 'pg'), true, 'inside a tagged quote');
});
