import { maskLiterals, type Dialect } from './text';

/** A table that the statement at the cursor reads or writes, with its alias when it has one. */
export interface Relation {
  /** `users`, or `public.users` as written. */
  path: string[];
  alias: string | null;
}

export const JOIN_MODS = new Set(['inner', 'left', 'right', 'full', 'cross', 'natural', 'outer', 'straight_join', 'lateral']);
const CLAUSE = new Set([
  'where', 'group', 'order', 'having', 'limit', 'offset', 'fetch', 'for', 'returning', 'window', 'qualify',
  'union', 'intersect', 'except', 'set', 'values', 'on', 'using', 'asc', 'desc', 'nulls', 'collate',
]);
/** Words that end a table reference instead of naming its alias; completion also quotes them as names. */
export const NOT_ALIAS = new Set([
  ...JOIN_MODS,
  ...CLAUSE,
  'select', 'from', 'join', 'update', 'into', 'as', 'and', 'or', 'not', 'in', 'is', 'like', 'ilike',
  'between', 'case', 'when', 'then', 'else', 'end', 'null', 'true', 'false', 'distinct', 'all', 'any',
  'some', 'exists', 'over', 'partition', 'by', 'with', 'recursive', 'interval', 'cast', 'only',
]);

export interface Token {
  value: string;
  kind: 'id' | 'punct';
}

export function tokenize(text: string): Token[] {
  const re = /"(?:[^"]|"")*"|`(?:[^`]|``)*`|\[[^\]]*\]|[A-Za-z_][\w$]*|\d+(?:\.\d+)?|\.|\(|\)|,|;|[^\s]/g;
  const out: Token[] = [];
  for (const m of text.matchAll(re)) {
    const raw = m[0];
    const quote = raw[0];
    if (quote === '"' || quote === '`') out.push({ value: raw.slice(1, -1).replace(quote === '"' ? /""/g : /``/g, quote), kind: 'id' });
    else if (quote === '[') out.push({ value: raw.slice(1, -1), kind: 'id' });
    else if (/^[A-Za-z_]/.test(raw)) out.push({ value: raw, kind: 'id' });
    else out.push({ value: raw, kind: 'punct' });
  }
  return out;
}

function readRef(tokens: Token[], start: number, columnList = false): { path: string[]; end: number } | null {
  let i = start;
  while (tokens[i]?.kind === 'id' && /^(only|lateral)$/i.test(tokens[i].value)) i++;
  if (tokens[i]?.kind !== 'id') return null;
  const path = [tokens[i].value];
  i++;
  while (tokens[i]?.value === '.' && tokens[i + 1]?.kind === 'id') {
    path.push(tokens[i + 1].value);
    i += 2;
  }
  // `generate_series(...)` and `(SELECT …) x` are not tables; their columns are unknown here. In
  // `INSERT INTO t (a, b)` the parentheses are the column list, not a call.
  return tokens[i]?.value === '(' && !columnList ? null : { path, end: i };
}

function readAlias(tokens: Token[], start: number): { alias: string | null; end: number } {
  const first = tokens[start];
  if (first?.kind !== 'id') return { alias: null, end: start };
  if (first.value.toLowerCase() === 'as' && tokens[start + 1]?.kind === 'id') return { alias: tokens[start + 1].value, end: start + 2 };
  if (!NOT_ALIAS.has(first.value.toLowerCase())) return { alias: first.value, end: start + 1 };
  return { alias: null, end: start };
}

/**
 * The tables of one statement: `FROM`/`JOIN`, `UPDATE`, `INSERT INTO`. Only the statement's own
 * level counts, so a subquery or a CTE body does not leak its tables into the list.
 */
export function relationsIn(sql: string, dialect: Dialect): Relation[] {
  const tokens = tokenize(maskLiterals(sql, dialect));
  const out: Relation[] = [];
  const seen = new Set<string>();
  let inFrom = false;
  let expectRef = false;
  let refColumnList = false;
  let depth = 0;
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    if (token.value === '(') {
      // A derived table or a function call: no table name to read, and its alias is not a relation.
      expectRef = false;
      refColumnList = false;
      depth++;
      continue;
    }
    if (token.value === ')') {
      depth = Math.max(0, depth - 1);
      continue;
    }
    if (depth > 0) continue;
    if (token.kind === 'punct') {
      // `FROM a, b` lists another table; a comma in an expression is inside its call's parens.
      if (token.value === ',' && inFrom) expectRef = true;
      continue;
    }
    const word = token.value.toLowerCase();
    if (word === 'from' || word === 'join' || word === 'update' || word === 'into') {
      inFrom = true;
      expectRef = true;
      refColumnList = word === 'into';
      continue;
    }
    if (JOIN_MODS.has(word)) continue;
    if (expectRef) {
      expectRef = false;
      const ref = readRef(tokens, i, refColumnList);
      refColumnList = false;
      if (!ref) continue;
      const { alias, end } = readAlias(tokens, ref.end);
      const key = `${ref.path.join('.')}\0${alias ?? ''}`;
      if (!seen.has(key)) {
        seen.add(key);
        out.push({ path: ref.path, alias });
      }
      i = end - 1;
      continue;
    }
    if (inFrom && CLAUSE.has(word)) inFrom = false;
  }
  return out;
}
