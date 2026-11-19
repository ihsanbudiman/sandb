/**
 * SQL completion that reads the clause at the cursor, as DataGrip does: tables after `FROM`/`JOIN`,
 * whole join clauses from foreign keys after `JOIN`, the join condition after `ON`, typed columns of
 * the statement's tables in expressions, and the next clause keywords after a table or an expression.
 */

import { insertCompletionText, pickedCompletion, startCompletion, type Completion, type CompletionContext, type CompletionResult, type CompletionSource } from '@codemirror/autocomplete';
import { keywordCompletionSource, MySQL, PostgreSQL, SQLite, type SQLDialect } from '@codemirror/lang-sql';
import { syntaxTree } from '@codemirror/language';
import type { EditorView } from '@codemirror/view';
import type { ForeignKeyInfo, SnapshotTable } from '../api';
import { tableLabel } from '../tableId';
import { JOIN_MODS, NOT_ALIAS, relationsIn, tokenize, type Relation, type Token } from './columns';
import { maskLiterals, statementRangeAt, type Dialect } from './text';

export const SQL_DIALECTS: Record<Dialect, SQLDialect> = { pg: PostgreSQL, mysql: MySQL, sqlite: SQLite };

export interface Column {
  name: string;
  /** The column type as the database names it; '' when unknown. */
  type: string;
}

/** Foreign keys a table declares and the ones that point at it. */
export interface Links {
  foreignKeys: ForeignKeyInfo[];
  references: ForeignKeyInfo[];
}

export interface SqlCompletionOptions {
  dialect: Dialect;
  /** The snapshot tables: names, kinds and the columns the snapshot kept. */
  tables: SnapshotTable[];
  /** Columns of one table as written (`users`, `sales.orders`); [] when unknown. */
  columns: (path: string[]) => Promise<Column[]>;
  /** Foreign keys of one table; null when unknown. */
  links?: (path: string[]) => Promise<Links | null>;
  /** Where the table list stands: still loading, failed (with the reason), or loaded. */
  tablesState?: { status: 'loading' | 'error' | 'ok'; error?: string };
}

/** A completion with the table it comes from, drawn on the right of the row. */
export interface SqlCompletion extends Completion {
  origin?: string;
}

type Context = 'start' | 'table' | 'join' | 'on' | 'column' | 'afterTable' | 'afterExpr' | 'alias' | 'any';

// Boosts, high to low: suggestions built from the schema, then the clause keywords that fit, then names.
const B_LINK = 20;
const B_FIT = 10;
const B_COLUMN = 6;
const B_QUALIFIER = 2;
const B_FUNCTION = 0;
const B_KEYWORD = -5;

/** Words after which an expression starts, so a column comes next. */
const EXPR_START: Record<string, true> = {
  select: true, distinct: true, where: true, and: true, or: true, not: true, on: true, by: true, having: true, set: true,
  when: true, then: true, else: true, case: true, in: true, is: true, like: true, ilike: true, between: true, exists: true,
  returning: true, values: true, using: true, any: true, all: true, some: true, return: true,
};

/** Words that open a clause; the last one before the cursor says where the cursor is. */
const CLAUSES: Record<string, true> = {
  select: true, from: true, join: true, where: true, on: true, using: true, group: true, order: true, having: true,
  set: true, values: true, limit: true, offset: true, returning: true, update: true, into: true, window: true,
  union: true, intersect: true, except: true, delete: true, insert: true, table: true,
};

const STATEMENTS: Record<Dialect, string[]> = {
  pg: ['SELECT', 'SELECT * FROM', 'INSERT INTO', 'UPDATE', 'DELETE FROM', 'WITH', 'EXPLAIN', 'EXPLAIN ANALYZE', 'CREATE TABLE', 'CREATE INDEX', 'CREATE VIEW', 'ALTER TABLE', 'DROP TABLE', 'TRUNCATE TABLE', 'BEGIN', 'COMMIT', 'ROLLBACK'],
  mysql: ['SELECT', 'SELECT * FROM', 'INSERT INTO', 'UPDATE', 'DELETE FROM', 'WITH', 'EXPLAIN', 'SHOW TABLES', 'DESCRIBE', 'CREATE TABLE', 'CREATE INDEX', 'CREATE VIEW', 'ALTER TABLE', 'DROP TABLE', 'TRUNCATE TABLE', 'BEGIN', 'COMMIT', 'ROLLBACK'],
  sqlite: ['SELECT', 'SELECT * FROM', 'INSERT INTO', 'UPDATE', 'DELETE FROM', 'WITH', 'EXPLAIN QUERY PLAN', 'CREATE TABLE', 'CREATE INDEX', 'CREATE VIEW', 'ALTER TABLE', 'DROP TABLE', 'PRAGMA', 'BEGIN', 'COMMIT', 'ROLLBACK'],
};

const AFTER_FROM = ['WHERE', 'JOIN', 'INNER JOIN', 'LEFT JOIN', 'RIGHT JOIN', 'CROSS JOIN', 'GROUP BY', 'ORDER BY', 'HAVING', 'LIMIT', 'UNION', 'UNION ALL'];
const AFTER_EXPR: Record<string, string[]> = {
  select: ['FROM', 'AS'],
  where: ['AND', 'OR', 'IS NULL', 'IS NOT NULL', 'IN', 'NOT IN', 'LIKE', 'BETWEEN', 'GROUP BY', 'ORDER BY', 'LIMIT'],
  on: ['AND', 'OR', 'WHERE', 'JOIN', 'LEFT JOIN', 'GROUP BY', 'ORDER BY'],
  using: ['WHERE', 'JOIN', 'LEFT JOIN', 'GROUP BY', 'ORDER BY'],
  group: ['HAVING', 'ORDER BY', 'LIMIT'],
  having: ['AND', 'OR', 'ORDER BY', 'LIMIT'],
  order: ['ASC', 'DESC', 'NULLS FIRST', 'NULLS LAST', 'LIMIT', 'OFFSET'],
  set: ['WHERE', 'RETURNING'],
  limit: ['OFFSET'],
};
/** Keywords after which completion opens again by itself, because a table or a column comes next. */
const REOPEN = /^(?:SELECT|FROM|(?:\w+ )*JOIN|UPDATE|INSERT INTO|DELETE FROM|WHERE|ON|AND|OR|GROUP BY|ORDER BY|HAVING|SET|TRUNCATE TABLE|ALTER TABLE|DROP TABLE|SELECT \* FROM|DESCRIBE)$/;

/** Short words that expand to a statement start at the beginning of a statement (`sel` → `SELECT * FROM `). */
export const SNIPPETS: { label: string; text: string }[] = [
  { label: 'sel', text: 'SELECT * FROM ' },
  { label: 'selc', text: 'SELECT COUNT(*) FROM ' },
  { label: 'ins', text: 'INSERT INTO ' },
  { label: 'upd', text: 'UPDATE ' },
  { label: 'del', text: 'DELETE FROM ' },
];
// Above the fitting keywords, so `sel` picks the snippet before `SELECT`.
const B_SNIPPET = 15;

/** Wait after a typed space, comma or `(` before the list opens by itself. */
export const AUTO_OPEN_DELAY = 200;

/**
 * True when the list should open by itself, as Ctrl+Space would, after the text typed up to `pos`:
 * a space, comma or `(` right after a word or symbol, with nothing but a line end, space or `)` after
 * the cursor. It stays shut for indentation, a second space, text in the middle of a line, after `;`,
 * and after `AS` (a new alias name comes next). Strings and comments get no list from the source.
 */
export function shouldAutoOpen(doc: string, pos: number, dialect: Dialect): boolean {
  const last = doc.slice(pos - 1, pos);
  if (last !== ' ' && last !== ',' && last !== '(') return false;
  const prev = doc.slice(pos - 2, pos - 1);
  if (last === ' ' && (!prev || /\s/.test(prev) || prev === ';')) return false;
  // Only at the end of what is written on the line (closing brackets aside): never while editing in the middle.
  const lineEnd = doc.indexOf('\n', pos);
  if (!/^[\s)]*$/.test(doc.slice(pos, lineEnd < 0 ? doc.length : lineEnd))) return false;
  const range = statementRangeAt(doc, pos, dialect);
  const before = doc.slice(range?.[0] ?? 0, pos);
  if (!before.trim()) return false;
  return scan(before, dialect).context !== 'alias';
}

function snippet(s: { label: string; text: string }): SqlCompletion {
  return {
    label: s.label,
    detail: s.text.trim(),
    type: 'snippet',
    boost: B_SNIPPET,
    apply: (view: EditorView, completion: Completion, from: number, to: number) => {
      view.dispatch({ ...insertCompletionText(view.state, s.text, from, to), annotations: pickedCompletion.of(completion) });
      // A table comes next: show the table list at once.
      startCompletion(view);
    },
  };
}

/** The one row the list shows when no table is known, so an empty list says why. Picking it inserts nothing. */
function tablesNote(opts: SqlCompletionOptions): SqlCompletion {
  const s = opts.tablesState;
  const label = s?.status === 'loading' ? 'Loading tables…' : s?.status === 'error' ? `Tables could not load: ${s.error ?? 'unknown error'}` : 'No tables in this database';
  return { label, type: 'text', apply: () => {} };
}

const COMMON_FUNCTIONS = ['count', 'sum', 'avg', 'min', 'max', 'coalesce', 'nullif', 'lower', 'upper', 'length', 'trim', 'replace', 'substr', 'round', 'abs', 'row_number', 'rank', 'dense_rank', 'lag', 'lead'];
const FUNCTIONS: Record<Dialect, string[]> = {
  pg: [...COMMON_FUNCTIONS, 'now', 'concat', 'greatest', 'least', 'substring', 'date_trunc', 'date_part', 'age', 'to_char', 'to_date', 'to_timestamp', 'string_agg', 'array_agg', 'json_agg', 'jsonb_agg', 'json_build_object', 'jsonb_build_object', 'generate_series', 'split_part', 'regexp_replace', 'gen_random_uuid', 'char_length', 'floor', 'ceil'],
  mysql: [...COMMON_FUNCTIONS, 'now', 'concat', 'concat_ws', 'greatest', 'least', 'substring', 'ifnull', 'if', 'group_concat', 'date_format', 'date_add', 'date_sub', 'datediff', 'curdate', 'str_to_date', 'unix_timestamp', 'from_unixtime', 'json_extract', 'json_object', 'json_arrayagg', 'uuid', 'char_length', 'floor', 'ceil'],
  sqlite: [...COMMON_FUNCTIONS, 'ifnull', 'iif', 'group_concat', 'total', 'instr', 'printf', 'date', 'time', 'datetime', 'julianday', 'strftime', 'json_extract', 'json_object', 'json_group_array', 'typeof', 'random'],
};

const IDENT = /^[\w$]*$/;

/** `name` as the dialect must write it: quoted when it has other characters, upper case on PG, or is a keyword. */
export function quoteIdent(name: string, dialect: Dialect): string {
  const plain = dialect === 'pg' ? /^[a-z_][a-z\d_$]*$/ : /^[A-Za-z_][\w$]*$/;
  if (plain.test(name) && !NOT_ALIAS.has(name.toLowerCase()) && name.toLowerCase() !== 'user') return name;
  const q = dialect === 'mysql' ? '`' : '"';
  return q + name.replaceAll(q, q + q) + q;
}

const quotePath = (path: string[], dialect: Dialect) => path.map((p) => quoteIdent(p, dialect)).join('.');

/** Short alias from the initials of the words of a table name (`order_items` → `oi`), unique in the statement. */
export function makeAlias(name: string, taken: Set<string>): string {
  const words = name.split(/[^A-Za-z\d]+|(?<=[a-z\d])(?=[A-Z])/).filter(Boolean);
  let base = words.map((w) => w[0]).join('').toLowerCase();
  if (!/^[a-z]/.test(base)) base = 't' + base;
  let alias = base;
  for (let n = 1; taken.has(alias) || NOT_ALIAS.has(alias) || alias === 'as'; n++) alias = base + n;
  return alias;
}

interface Scan {
  context: Context;
  /** The last clause keyword at the cursor's level, lower case. */
  clause: string;
}

/** Where the cursor stands, from the tokens before it at its own parenthesis level. */
export function scan(before: string, dialect: Dialect): Scan {
  const levels: Token[][] = [[]];
  for (const t of tokenize(maskLiterals(before, dialect))) {
    if (t.value === '(') {
      levels.at(-1)!.push(t);
      levels.push([]);
    } else if (t.value === ')') {
      if (levels.length > 1) levels.pop();
      levels.at(-1)!.push(t);
    } else levels.at(-1)!.push(t);
  }
  const cur = levels.at(-1)!;
  const nested = levels.length > 1;
  const word = (t: Token | undefined) => (t?.kind === 'id' ? t.value.toLowerCase() : null);
  let clause = '';
  for (let i = cur.length - 1; i >= 0; i--) {
    const w = word(cur[i]);
    if (w && CLAUSES[w]) {
      clause = w;
      break;
    }
  }
  const prev = cur.at(-1);
  const prevWord = word(prev);
  if (!prev) return { context: nested ? 'column' : 'start', clause };
  if (prevWord === 'as') return { context: 'alias', clause };
  if (prevWord === 'join') return { context: 'join', clause };
  if (prevWord === 'table' && word(cur.at(-2)) === 'create') return { context: 'any', clause };
  if (prevWord === 'from' || prevWord === 'update' || prevWord === 'into' || prevWord === 'table' || prevWord === 'describe') return { context: 'table', clause };
  if (prev.value === ',' && clause === 'from') return { context: 'table', clause };
  if (clause === 'on' && (prevWord === 'on' || prevWord === 'and' || prevWord === 'or')) return { context: 'on', clause };
  const atom = prev.value === ')' || (prev.kind === 'id' && !EXPR_START[prevWord!]) || /^\d/.test(prev.value);
  if (clause === 'from' || clause === 'join' || clause === 'update' || clause === 'into' || clause === 'table') {
    // `FROM t`, `FROM t a`, `JOIN t a`: the reference is complete; `LEFT` waits for `JOIN`.
    if (prevWord && JOIN_MODS.has(prevWord)) return { context: 'afterTable', clause };
    return { context: atom ? 'afterTable' : 'any', clause };
  }
  if (prev.value === '(' || prev.value === ',' || !atom || /^[=<>!+\-*/%|]+$/.test(prev.value)) {
    return { context: clause || nested ? 'column' : 'any', clause };
  }
  return { context: 'afterExpr', clause };
}

function keyword(label: string, boost: number): SqlCompletion {
  const reopen = REOPEN.test(label);
  return {
    label,
    type: 'keyword',
    boost,
    apply: (view: EditorView, completion: Completion, from: number, to: number) => {
      const space = /\s/.test(view.state.sliceDoc(to, to + 1)) ? '' : ' ';
      view.dispatch({ ...insertCompletionText(view.state, label + space, from, to), annotations: pickedCompletion.of(completion) });
      if (reopen) startCompletion(view);
    },
  };
}

function fn(name: string): SqlCompletion {
  return {
    label: name,
    type: 'function',
    boost: B_FUNCTION,
    apply: (view: EditorView, completion: Completion, from: number, to: number) => {
      const call = view.state.sliceDoc(to, to + 1) !== '(';
      view.dispatch({ ...insertCompletionText(view.state, call ? `${name}()` : name, from, to), annotations: pickedCompletion.of(completion) });
      if (call) view.dispatch({ selection: { anchor: view.state.selection.main.head - 1 } });
    },
  };
}

/** The relation a table key of the snapshot or a foreign key names, compared as the database would. */
function sameTable(rel: string[], schema: string | undefined, name: string, dialect: Dialect): boolean {
  const eq = (a: string, b: string) => (dialect === 'pg' ? a === b : a.toLowerCase() === b.toLowerCase());
  if (!eq(rel[rel.length - 1], name)) return false;
  return rel.length < 2 || !schema || eq(rel[rel.length - 2], schema);
}

/** The path a foreign key side is written with: the schema only when it is not PG's `public`. */
const fkPath = (schema: string | undefined, table: string) => (schema && schema !== 'public' ? [schema, table] : [table]);

const qualifierOf = (rel: Relation) => rel.alias ?? rel.path[rel.path.length - 1];

const condition = (a: string, aCols: string[], b: string, bCols: string[], d: Dialect) =>
  aCols.map((c, i) => `${quoteIdent(a, d)}.${quoteIdent(c, d)} = ${quoteIdent(b, d)}.${quoteIdent(bCols[i], d)}`).join(' AND ');

/** `orders o ON o.user_id = u.id` for each table that a foreign key links to a table of the statement. */
async function joinClauses(relations: Relation[], opts: SqlCompletionOptions): Promise<SqlCompletion[]> {
  if (!opts.links || !relations.length) return [];
  const d = opts.dialect;
  const taken = new Set(relations.flatMap((r) => [qualifierOf(r).toLowerCase(), r.path[r.path.length - 1].toLowerCase()]));
  const all = await Promise.all(relations.map(async (rel) => ({ rel, links: await opts.links!(rel.path) })));
  const out: SqlCompletion[] = [];
  const seen = new Set<string>();
  for (const { rel, links } of all) {
    if (!links) continue;
    const q = qualifierOf(rel);
    const targets = [
      ...links.foreignKeys.map((fk) => ({ path: fkPath(fk.refSchema, fk.refTable), mine: fk.columns, theirs: fk.refColumns })),
      ...links.references.map((fk) => ({ path: fkPath(fk.schema, fk.table), mine: fk.refColumns, theirs: fk.columns })),
    ];
    for (const t of targets) {
      const name = t.path[t.path.length - 1];
      const alias = makeAlias(name, taken);
      const label = `${quotePath(t.path, d)} ${alias} ON ${condition(alias, t.theirs, q, t.mine, d)}`;
      if (seen.has(label)) continue;
      seen.add(label);
      out.push({ label, type: 'join', boost: B_LINK, origin: `→ ${q}` });
    }
  }
  return out;
}

/** The join conditions between the last table of the statement and the tables before it. */
async function onConditions(relations: Relation[], opts: SqlCompletionOptions): Promise<SqlCompletion[]> {
  const last = relations.at(-1);
  if (!opts.links || !last || relations.length < 2) return [];
  const links = await opts.links(last.path);
  if (!links) return [];
  const d = opts.dialect;
  const q = qualifierOf(last);
  const out: SqlCompletion[] = [];
  for (const other of relations.slice(0, -1)) {
    const o = qualifierOf(other);
    for (const fk of links.foreignKeys)
      if (sameTable(other.path, fk.refSchema, fk.refTable, d)) out.push({ label: condition(q, fk.columns, o, fk.refColumns, d), type: 'join', boost: B_LINK, origin: fk.name });
    for (const fk of links.references)
      if (sameTable(other.path, fk.schema, fk.table, d)) out.push({ label: condition(q, fk.refColumns, o, fk.columns, d), type: 'join', boost: B_LINK, origin: fk.name });
  }
  return out;
}

function tableOptions(opts: SqlCompletionOptions): SqlCompletion[] {
  const d = opts.dialect;
  const out: SqlCompletion[] = opts.tables.map((t) => {
    const label = tableLabel(t);
    const apply = quotePath(label === t.name ? [t.name] : [t.schema!, t.name], d);
    return { label, type: t.kind === 'table' ? 'table' : 'view', boost: B_FIT, origin: t.kind === 'table' ? (t.schema ?? '') : t.kind, ...(apply !== label ? { apply } : {}) };
  });
  // PG schemas other than `public`: picking one writes `schema.` and lists its tables.
  for (const s of new Set(opts.tables.map((t) => t.schema).filter((s): s is string => !!s && s !== 'public'))) {
    out.push({
      label: s,
      type: 'schema',
      boost: B_QUALIFIER,
      apply: (view: EditorView, completion: Completion, from: number, to: number) => {
        view.dispatch({ ...insertCompletionText(view.state, quoteIdent(s, d) + '.', from, to), annotations: pickedCompletion.of(completion) });
        startCompletion(view);
      },
    });
  }
  return out;
}

/** Typed columns of the statement's tables. A name two tables share gets its qualifier. */
async function scopeColumns(relations: Relation[], opts: SqlCompletionOptions, explicit: boolean, boost: number): Promise<SqlCompletion[]> {
  let groups: { rel: Relation; columns: Column[] }[];
  if (relations.length) {
    groups = await Promise.all(relations.slice(0, 500).map(async (rel) => ({ rel, columns: await opts.columns(rel.path) })));
  } else if (explicit) {
    // No table yet: the columns of the whole database, on an explicit request only.
    groups = opts.tables
      .filter((t) => t.columns.length)
      .map((t) => ({ rel: { path: tableLabel(t).split('.'), alias: null }, columns: t.columns.map((c) => ({ name: c.name, type: c.dataType })) }));
  } else return [];
  const d = opts.dialect;
  const count = new Map<string, number>();
  for (const g of groups) for (const c of g.columns) count.set(c.name, (count.get(c.name) ?? 0) + 1);
  const out: SqlCompletion[] = [];
  const seen = new Set<string>();
  for (const { rel, columns } of groups) {
    const q = qualifierOf(rel);
    for (const c of columns) {
      const key = `${q}\0${c.name}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const name = quoteIdent(c.name, d);
      // A name that two tables share must be qualified, or the database refuses the query.
      const apply = count.get(c.name)! > 1 ? `${quoteIdent(q, d)}.${name}` : name;
      out.push({ label: c.name, type: 'column', detail: c.type, origin: rel.alias ? `${rel.alias} (${rel.path.join('.')})` : rel.path.join('.'), boost, ...(apply !== c.name ? { apply } : {}) });
      if (out.length >= 2000) return out;
    }
  }
  return out;
}

/** Aliases and table names of the statement, for `alias.` qualifiers. */
function qualifiers(relations: Relation[]): SqlCompletion[] {
  const seen = new Set<string>();
  return relations.flatMap((rel) => {
    const q = qualifierOf(rel);
    if (seen.has(q)) return [];
    seen.add(q);
    return [{ label: q, type: 'alias', origin: rel.alias ? rel.path.join('.') : '', boost: B_QUALIFIER } satisfies SqlCompletion];
  });
}

/** After `x.`: the columns of the table or alias `x`, or the tables of PG schema `x`. */
async function dotted(ctx: CompletionContext, relations: Relation[], opts: SqlCompletionOptions): Promise<CompletionResult | null> {
  const line = ctx.state.sliceDoc(ctx.state.doc.lineAt(ctx.pos).from, ctx.pos);
  const m = /((?:(?:[A-Za-z_][\w$]*|"[^"]+"|`[^`]+`)\.)+)([\w$]*)$/.exec(line);
  if (!m) return null;
  let path = m[1].slice(0, -1).match(/"[^"]+"|`[^`]+`|[^.]+/g)!.map((p) => p.replace(/^(["`])(.*)\1$/, '$2'));
  const from = ctx.pos - m[2].length;
  if (path.length === 1) {
    const rel = relations.find((r) => r.alias === path[0]);
    if (rel) path = rel.path;
    else if (opts.tables.some((t) => t.schema === path[0]) && !opts.tables.some((t) => tableLabel(t) === path[0])) {
      const options = opts.tables
        .filter((t) => t.schema === path[0])
        .map((t): SqlCompletion => ({ label: t.name, type: t.kind === 'table' ? 'table' : 'view', origin: t.kind === 'table' ? '' : t.kind, ...(quoteIdent(t.name, opts.dialect) !== t.name ? { apply: quoteIdent(t.name, opts.dialect) } : {}) }));
      return { from, options, validFor: IDENT };
    }
  }
  const columns = await opts.columns(path);
  if (!columns.length || ctx.aborted) return null;
  const options = columns.map((c): SqlCompletion => {
    const apply = quoteIdent(c.name, opts.dialect);
    return { label: c.name, type: 'column', detail: c.type, ...(apply !== c.name ? { apply } : {}) };
  });
  return { from, options, validFor: IDENT };
}

const keywordSources = new Map<Dialect, CompletionSource>();

/** The lang-sql keywords and types of the dialect, upper case, below everything else. */
function dialectKeywords(ctx: CompletionContext, d: Dialect): SqlCompletion[] {
  let src = keywordSources.get(d);
  if (!src) keywordSources.set(d, (src = keywordCompletionSource(SQL_DIALECTS[d], true, (label, type) => ({ label, type, boost: B_KEYWORD }))));
  const r = src(ctx);
  return r && !(r instanceof Promise) ? (r.options as SqlCompletion[]) : [];
}

/** First wins: a fitting keyword phrase hides the plain lang-sql keyword with the same label. */
function merge(...lists: SqlCompletion[][]): SqlCompletion[] {
  const seen = new Set<string>();
  const out: SqlCompletion[] = [];
  for (const list of lists)
    for (const o of list) {
      const key = `${o.type}\0${o.label}`;
      if (o.type === 'keyword' || o.type === 'type' ? seen.has(o.label) : seen.has(key)) continue;
      seen.add(o.type === 'keyword' || o.type === 'type' ? o.label : key);
      out.push(o);
    }
  return out;
}

/** The completion source of a SQL editor. */
export async function sqlCompletion(ctx: CompletionContext, opts: SqlCompletionOptions): Promise<CompletionResult | null> {
  const { state, pos } = ctx;
  const node = syntaxTree(state).resolveInner(pos, -1);
  if (node.name === 'String' || node.name === 'LineComment' || node.name === 'BlockComment' || node.name === 'QuotedIdentifier') return null;
  const doc = state.doc.toString();
  const range = statementRangeAt(doc, pos, opts.dialect);
  const start = range?.[0] ?? pos;
  const sql = range ? doc.slice(range[0], range[1]) : '';
  const relations = sql ? relationsIn(sql, opts.dialect) : [];

  const word = ctx.matchBefore(/[\w$]+/);
  const wordFrom = word?.from ?? pos;
  if (state.sliceDoc(wordFrom - 1, wordFrom) === '.') return dotted(ctx, relations, opts);
  // Typing opens the list on a word; Ctrl+Space and a picked keyword open it on an empty one.
  if (!word && !ctx.explicit) return null;

  const { context, clause } = scan(doc.slice(start, wordFrom), opts.dialect);
  const d = opts.dialect;
  const kw = () => dialectKeywords(ctx, d);
  // A small falling boost keeps the written order (most used first, e.g. `WHERE` before `CROSS JOIN`) instead of A–Z.
  const phrases = (list: string[]) => list.map((l, i) => keyword(l, B_FIT - i * 0.01));
  let options: SqlCompletion[];
  switch (context) {
    case 'alias':
      return null;
    case 'start':
      // Snippets match only their own short word, so they do not crowd the list on `s` or `d`.
      options = merge(word && SNIPPETS.some((s) => s.label.startsWith(word.text.toLowerCase()) && word.text.length >= 3) ? SNIPPETS.map(snippet) : [], phrases(STATEMENTS[d]), kw());
      break;
    case 'table':
      options = tableOptions(opts);
      if (!options.length) options = [tablesNote(opts)];
      break;
    case 'join':
      options = merge(await joinClauses(relations, opts), tableOptions(opts));
      if (!options.length) options = [tablesNote(opts)];
      break;
    case 'on':
      options = merge(await onConditions(relations, opts), await scopeColumns(relations, opts, ctx.explicit, B_COLUMN), qualifiers(relations), FUNCTIONS[d].map(fn));
      break;
    case 'afterTable': {
      const next = clause === 'join' ? ['ON', 'USING', ...AFTER_FROM, 'AS'] : clause === 'update' ? ['SET', 'AS'] : clause === 'into' ? ['VALUES', 'SELECT', 'DEFAULT VALUES'] : clause === 'table' ? [] : [...AFTER_FROM, 'AS'];
      options = merge(phrases(next), kw());
      break;
    }
    case 'afterExpr':
      // The expression is complete: the next clause comes first, names stay reachable by typing.
      options = merge(phrases(AFTER_EXPR[clause] ?? []), await scopeColumns(relations, opts, ctx.explicit, B_KEYWORD + 1), kw(), FUNCTIONS[d].map((f) => ({ ...fn(f), boost: B_KEYWORD })));
      break;
    case 'column':
      options = merge(
        await scopeColumns(relations, opts, ctx.explicit, B_COLUMN),
        qualifiers(relations),
        FUNCTIONS[d].map(fn),
        clause === 'select' || clause === '' ? phrases(['DISTINCT', 'CASE', '*']).map((o) => ({ ...o, boost: B_KEYWORD + 1 })) : [],
        kw(),
      );
      break;
    default:
      options = merge(await scopeColumns(relations, opts, ctx.explicit, B_COLUMN), tableOptions(opts).map((o) => ({ ...o, boost: B_QUALIFIER })), FUNCTIONS[d].map(fn), kw());
  }
  if (ctx.aborted || !options.length) return null;
  // At a statement start the snippets depend on the typed word, so the list asks again on each key.
  return { from: wordFrom, options, validFor: context === 'start' ? undefined : IDENT };
}
