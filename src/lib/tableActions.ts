import type { ColumnInfo, ConstraintInfo, Engine, ForeignKeyInfo, IndexInfo, TableKind } from './api';
import type { Conn } from './state.svelte';
import { quoteIdent } from './results';

/** A table, view or materialized view from the connection tree. `schema` is null on MySQL and SQLite. */
export type TableRef = { database: string; schema: string | null; name: string; kind: TableKind };

export const kindLabel: Record<TableKind, string> = { table: 'table', view: 'view', matview: 'materialized view' };

export type TableAction = 'view' | 'count' | 'ddl' | 'select' | 'insert' | 'exportDdl' | 'exportSql' | 'exportCsv' | 'exportDocuments' | 'truncate' | 'drop';

/** A Truncate or Drop that waits for the user to confirm it. */
export type Danger = { action: 'truncate' | 'drop'; c: Conn; t: TableRef };

export const qualified = (e: Engine, t: TableRef) => (t.schema ? quoteIdent(e, t.schema) + '.' : '') + quoteIdent(e, t.name);

/** Unquoted, for labels and the clipboard. */
export const displayName = (t: TableRef) => (t.schema ? t.schema + '.' : '') + t.name;

export type TableFilterOperator = '=' | '<>' | '>' | '>=' | '<' | '<=' | 'contains' | 'startsWith' | 'endsWith' | 'isNull' | 'isNotNull';
export type TableFilter = { id: string; column: string; operator: TableFilterOperator; value: string };

const NUMERIC = /^(int|integer|smallint|tinyint|mediumint|bigint|int[248]|serial|bigserial|smallserial|real|double|float|float[48]|decimal|numeric|number|money)\b/i;
const NUMBER = /^-?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i;

function literal(engine: Engine, value: string, dataType: string) {
  const v = value.trim();
  if (NUMERIC.test(dataType) && NUMBER.test(v)) return v;
  if (/bool/i.test(dataType) && /^(true|false|1|0)$/i.test(v)) {
    if (engine === 'pg') return /^(true|1)$/i.test(v) ? 'TRUE' : 'FALSE';
    return /^(true|1)$/i.test(v) ? '1' : '0';
  }
  let s = value.replace(/'/g, "''");
  if (engine === 'mysql') s = s.replace(/\\/g, '\\\\');
  return `'${s}'`;
}

/** SQL for one table Data view run. Values are literals because the existing query API accepts SQL text. */
export function tableDataSql(engine: Engine, table: TableRef, filters: TableFilter[], columns: string[] = [], types: string[] = []) {
  if (engine === 'mongo') {
    // mongosh: db.<collection>.find({ ... }). Fields with several filters cannot share one key,
    // so their conditions go through $and; the others merge into one document.
    const counts = new Map<string, number>();
    for (const f of filters) counts.set(f.column, (counts.get(f.column) ?? 0) + 1);
    const plain: string[] = [];
    const ands: string[] = [];
    for (const f of filters) {
      // A sampled field can have several types (`decimal128 | int32 | string`); the most common one types the value.
      const cond = mongoCondition(f, (types[columns.indexOf(f.column)] ?? '').split(' | ')[0]);
      (counts.get(f.column) ?? 0) > 1 ? ands.push(`{ ${cond} }`) : plain.push(cond);
    }
    const doc = [plain.join(', '), ands.length ? `$and: [${ands.join(', ')}]` : ''].filter(Boolean).join(', ');
    return `db.${mongoName(table.name)}.find({${doc ? ` ${doc} ` : ''}})`;
  }
  const where = filters.map((f) => {
    const col = quoteIdent(engine, f.column);
    if (f.operator === 'isNull') return `${col} IS NULL`;
    if (f.operator === 'isNotNull') return `${col} IS NOT NULL`;
    if (['contains', 'startsWith', 'endsWith'].includes(f.operator)) {
      const value = f.operator === 'contains' ? `%${f.value}%` : f.operator === 'startsWith' ? `${f.value}%` : `%${f.value}`;
      return `${col} LIKE ${literal(engine, value, 'text')}`;
    }
    return `${col} ${f.operator} ${literal(engine, f.value, types[columns.indexOf(f.column)] ?? '')}`;
  });
  return `SELECT *\nFROM ${qualified(engine, table)}${where.length ? `\nWHERE ${where.join(' AND ')}` : ''}`;
}

// ---- MongoDB: the Data view runs as a mongosh find() ----------------------------------------

/** A plain JS identifier: these keys stay bare in the filter document, anything else gets JSON quotes. */
export const PLAIN_IDENT = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

/** Mongo operator of a comparison filter. */
const MONGO_OP: Partial<Record<TableFilterOperator, string>> = { '>': '$gt', '>=': '$gte', '<': '$lt', '<=': '$lte', '<>': '$ne' };

/**
 * The collection access after `db.`, always through getCollection(): a bare `db.<name>` can hit a
 * database method (`db.version`), and a name may hold quotes, dots or non-ASCII text.
 */
export function mongoName(name: string) {
  return `getCollection(${JSON.stringify(name)})`;
}

const INT64_MIN = -9223372036854775808n;
const INT64_MAX = 9223372036854775807n;

/**
 * Exact decimal digits of a numeric string when its value is a whole number (`1e18` gives
 * `1000000000000000000`, `2.0` gives `2`); null when it has a fraction.
 */
function integerDigits(v: string): string | null {
  const m = /^(-?)(\d*)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/.exec(v);
  if (!m) return null;
  const [, sign, whole, fraction = '', exponent = '0'] = m;
  const digits = whole + fraction;
  if (!digits) return null;
  // The decimal point of `whole.fraction` sits after `whole`; the exponent moves it.
  const point = whole.length + Number(exponent);
  // int64 has at most 19 digits: a longer integer is out of range, and there is no point in building it.
  if (point > 20) return null;
  if (point <= 0) return /^0+$/.test(digits) ? '0' : null;
  if (point < digits.length && !/^0+$/.test(digits.slice(point))) return null;
  const all = point >= digits.length ? digits + '0'.repeat(point - digits.length) : digits.slice(0, point);
  const trimmed = all.replace(/^0+(?=\d)/, '');
  return sign ? `-${trimmed}` : trimmed;
}

/** Text of one filter value, typed by the column's first data type. */
function mongoValue(value: string, t: string) {
  const v = value.trim();
  // An int64 past 2^53 has no exact double: keep its digits in a NumberLong.
  if (t === 'int64' && NUMBER.test(v)) {
    const digits = integerDigits(v);
    if (digits !== null) {
      const n = BigInt(digits);
      if (n < INT64_MIN || n > INT64_MAX) throw new Error(`${v} does not fit in an int64 (${INT64_MIN} to ${INT64_MAX})`);
      return n > 9007199254740991n || n < -9007199254740991n ? `NumberLong("${digits}")` : digits;
    }
  }
  if (t === 'decimal128' && NUMBER.test(v)) return `NumberDecimal("${v}")`;
  if (t === 'objectId' && /^[0-9a-f]{24}$/i.test(v)) return `ObjectId("${v}")`;
  // The value is arbitrary text: quote it, so it cannot end the string or the statement.
  if (t === 'date') return `ISODate(${JSON.stringify(v)})`;
  if (t === 'bool' && /^(true|false)$/i.test(v)) return v.toLowerCase();
  if ((t === 'int32' || t === 'int64' || t === 'double' || t === 'number') && NUMBER.test(v)) return v;
  return JSON.stringify(v);
}

/** One filter as a `field: condition` piece of the filter document. */
function mongoCondition(f: TableFilter, type: string) {
  const key = PLAIN_IDENT.test(f.column) ? f.column : JSON.stringify(f.column);
  if (f.operator === 'isNull') return `${key}: null`;
  if (f.operator === 'isNotNull') return `${key}: { $ne: null }`;
  if (f.operator === 'contains' || f.operator === 'startsWith' || f.operator === 'endsWith') {
    // The pattern is a literal: escape the metacharacters, then anchor the ends that mean it.
    const esc = f.value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const pattern = f.operator === 'contains' ? esc : f.operator === 'startsWith' ? `^${esc}` : `${esc}$`;
    return `${key}: { $regex: ${JSON.stringify(pattern)} }`;
  }
  const v = mongoValue(f.value, type);
  if (f.operator === '=') return `${key}: ${v}`;
  if (f.operator === '<>') return `${key}: { $ne: ${v} }`;
  return `${key}: { ${MONGO_OP[f.operator]}: ${v} }`;
}

/** Applies the same operators to mock rows after parsing a generated table query. */
export function filterRows<T extends Record<string, unknown>>(rows: T[], filters: TableFilter[]): T[] {
  const text = (v: unknown) => typeof v === 'object' && v !== null ? JSON.stringify(v) : String(v);
  return rows.filter((row) => filters.every((f) => {
    const v = row[f.column];
    if (f.operator === 'isNull') return v == null;
    if (f.operator === 'isNotNull') return v != null;
    if (v == null) return false;
    if (f.operator === 'contains') return text(v).includes(f.value);
    if (f.operator === 'startsWith') return text(v).startsWith(f.value);
    if (f.operator === 'endsWith') return text(v).endsWith(f.value);
    const numeric = typeof v === 'number' && NUMBER.test(f.value.trim());
    const a = typeof v === 'boolean' ? v : numeric ? v : text(v);
    const b = typeof v === 'boolean' ? /^(true|1)$/i.test(f.value.trim()) : numeric ? Number(f.value) : f.value;
    if (f.operator === '=') return a === b;
    if (f.operator === '<>') return a !== b;
    if (f.operator === '>') return a > b;
    if (f.operator === '>=') return a >= b;
    if (f.operator === '<') return a < b;
    return a <= b;
  }));
}

export const selectSql = (e: Engine, t: TableRef, cols: ColumnInfo[]) =>
  `SELECT ${cols.map((c) => quoteIdent(e, c.name)).join(', ') || '*'}\nFROM ${qualified(e, t)};`;

// A value of the column's kind, so the template runs once the user edits it.
function placeholder(e: Engine, dataType: string) {
  if (/bool/i.test(dataType)) return e === 'pg' ? 'false' : '0';
  if (/int|serial|numeric|decimal|real|double|float|money|bit/i.test(dataType)) return '0';
  return "''";
}

export const insertSql = (e: Engine, t: TableRef, cols: ColumnInfo[]) =>
  `INSERT INTO ${qualified(e, t)} (${cols.map((c) => quoteIdent(e, c.name)).join(', ')})\nVALUES (${cols.map((c) => placeholder(e, c.dataType)).join(', ')});`;

/** SQLite has no TRUNCATE; a DELETE without WHERE uses its truncate optimization. */
export function dangerSql(e: Engine, t: TableRef, a: 'truncate' | 'drop') {
  if (a === 'drop') return `DROP ${kindLabel[t.kind].toUpperCase()} ${qualified(e, t)};`;
  return e === 'sqlite' ? `DELETE FROM ${qualified(e, t)};` : `TRUNCATE TABLE ${qualified(e, t)};`;
}

export const FK_RULES = ['NO ACTION', 'RESTRICT', 'CASCADE', 'SET NULL', 'SET DEFAULT'] as const;

/** The editable parts of one column, constraint, foreign key or index. `type`, `default`, `check` and `method` are SQL text the user typed. */
export type ColumnDef = { name: string; type: string; notNull: boolean; default: string };
export type ConstraintDef = { kind: 'primary' | 'unique' | 'check'; name: string; columns: string[]; check: string };
export type ForeignKeyDef = { name: string; columns: string[]; ref: TableRef; refColumns: string[]; onUpdate: string; onDelete: string };
export type IndexDef = { name: string; columns: string[]; unique: boolean; method: string };

/** One structure change from the Properties view. An edit holds the current object (`from`) and the wanted one (`to`). */
export type SchemaChange =
  | ({ op: 'addColumn' } & ColumnDef)
  | ({ op: 'addConstraint' } & ConstraintDef)
  | ({ op: 'addForeignKey' } & ForeignKeyDef)
  | ({ op: 'addIndex' } & IndexDef)
  | { op: 'editColumn'; from: ColumnDef; to: ColumnDef }
  | { op: 'editConstraint'; from: ConstraintDef; to: ConstraintDef }
  | { op: 'editForeignKey'; from: ForeignKeyDef; to: ForeignKeyDef }
  | { op: 'editIndex'; from: IndexDef; to: IndexDef }
  | { op: 'dropColumn' | 'dropForeignKey' | 'dropIndex'; name: string }
  | { op: 'dropConstraint'; kind: ConstraintInfo['kind']; name: string };

/** SQLite has no ALTER TABLE … ADD/DROP CONSTRAINT: constraints and foreign keys exist only in CREATE TABLE. */
export const canAlterConstraints = (e: Engine) => e !== 'sqlite';

const MYSQL_DEFAULT_SQL = /^(?:'.*'|\(.*\)|b'[01]*'|x'[0-9a-f]*'|[a-z_]+\(.*\)|current_(?:timestamp|date|time)|localtime(?:stamp)?)$/is;

/** The column as the edit form starts. MySQL reports a string default without quotes, so it gets them back. */
export function columnDef(e: Engine, c: ColumnInfo): ColumnDef {
  let def = c.default ?? '';
  if (e === 'mysql' && def && !MYSQL_DEFAULT_SQL.test(def)) def = literal(e, def, c.dataType);
  return { name: c.name, type: c.dataType, notNull: !c.nullable, default: def };
}

/** `null` for an exclusion constraint: the form cannot show it. PG gives the check as `CHECK (expr)`, MySQL as the bare expression. */
export function constraintDef(c: ConstraintInfo): ConstraintDef | null {
  if (c.kind === 'exclude') return null;
  const check = c.kind === 'check' ? (c.definition ?? '').replace(/^CHECK \(([\s\S]*)\)(?: NOT VALID)?$/, '$1') : '';
  return { kind: c.kind, name: c.name, columns: c.columns, check };
}

export const foreignKeyDef = (t: TableRef, f: ForeignKeyInfo): ForeignKeyDef => ({
  name: f.name,
  columns: f.columns,
  ref: { database: t.database, schema: f.refSchema ?? null, name: f.refTable, kind: 'table' },
  refColumns: f.refColumns,
  onUpdate: f.onUpdate,
  onDelete: f.onDelete,
});

/**
 * How far the form can edit an index. `full`: the columns, Unique and Method describe all of it, so a
 * drop and create keeps it. `rename`: it has more (expressions, DESC, WHERE, INCLUDE, FULLTEXT), so only the
 * name may change. `null`: neither (the primary key's index, or SQLite, which cannot rename an index).
 */
export function indexEdit(e: Engine, x: IndexInfo, columns: ColumnInfo[]): 'full' | 'rename' | null {
  if (x.primary) return null;
  const plain = x.columns.every((c) => columns.some((col) => col.name === c));
  let full = plain;
  if (e === 'mysql') full &&= !x.method || ['BTREE', 'HASH'].includes(x.method.toUpperCase());
  else {
    // The definition must end in the bare column list: `… (a, b)`.
    const list = x.definition?.match(/\(([^()]*)\)\s*$/)?.[1];
    full &&= list != null && list.split(',').map((s) => s.trim().replace(/^["`[](.*)["`\]]$/, '$1')).join('\0') === x.columns.join('\0');
  }
  return full ? 'full' : e === 'sqlite' ? null : 'rename';
}

export const indexDef = (e: Engine, x: IndexInfo): IndexDef => ({
  name: x.name,
  columns: x.columns,
  unique: x.unique,
  // PG always names the method; the form keeps it so an unchanged index stays the same.
  method: e === 'sqlite' ? '' : (x.method ?? ''),
});

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

export function schemaChangeSql(e: Engine, t: TableRef, c: SchemaChange): string {
  const q = (n: string) => quoteIdent(e, n);
  const list = (cols: string[]) => cols.map(q).join(', ');
  const alter = `ALTER TABLE ${qualified(e, t)}`;
  // An empty name lets the database pick one.
  const named = (n: string) => (n.trim() ? `CONSTRAINT ${q(n.trim())} ` : '');
  const rule = (on: string, r: string) => (r && r !== 'NO ACTION' ? ` ON ${on} ${r}` : '');
  const constraint = (d: ConstraintDef) => `${named(d.name)}${d.kind === 'check' ? `CHECK (${d.check.trim()})` : `${d.kind === 'primary' ? 'PRIMARY KEY' : 'UNIQUE'} (${list(d.columns)})`}`;
  const foreignKey = (d: ForeignKeyDef) =>
    `${named(d.name)}FOREIGN KEY (${list(d.columns)})\nREFERENCES ${qualified(e, d.ref)} (${list(d.refColumns)})${rule('UPDATE', d.onUpdate)}${rule('DELETE', d.onDelete)}`;
  const dropConstraint = (kind: ConstraintInfo['kind'], name: string) =>
    // A MySQL UNIQUE constraint is an index.
    e === 'mysql' && kind === 'primary' ? 'DROP PRIMARY KEY' : e === 'mysql' && kind === 'unique' ? `DROP INDEX ${q(name)}` : `DROP CONSTRAINT ${q(name)}`;
  const dropForeignKey = (name: string) => `DROP ${e === 'mysql' ? 'FOREIGN KEY' : 'CONSTRAINT'} ${q(name)}`;
  // A PG index lives in the table's schema; a MySQL index belongs to its table.
  const dropIndex = (name: string) => (e === 'mysql' ? `DROP INDEX ${q(name)} ON ${qualified(e, t)};` : `DROP INDEX ${t.schema ? q(t.schema) + '.' : ''}${q(name)};`);
  const createIndex = (d: IndexDef) => {
    // PG names the method before the column list, MySQL after it; SQLite has none.
    const head = `CREATE ${d.unique ? 'UNIQUE ' : ''}INDEX ${q(d.name.trim())}\nON ${qualified(e, t)}`;
    if (e === 'pg') return `${head}${d.method ? ` USING ${d.method}` : ''} (${list(d.columns)});`;
    return `${head} (${list(d.columns)})${e === 'mysql' && d.method ? ` USING ${d.method}` : ''};`;
  };
  // PG and SQLite DDL is transactional: several statements commit together or not at all.
  const atomic = (stmts: string[]) => (stmts.length > 1 ? ['BEGIN;', ...stmts, 'COMMIT;'] : stmts).join('\n');
  switch (c.op) {
    case 'addColumn':
      return `${alter}\nADD COLUMN ${q(c.name.trim())} ${c.type.trim()}${c.notNull ? ' NOT NULL' : ''}${c.default.trim() ? ` DEFAULT ${c.default.trim()}` : ''};`;
    case 'addConstraint':
      return `${alter}\nADD ${constraint(c)};`;
    case 'addForeignKey':
      return `${alter}\nADD ${foreignKey(c)};`;
    case 'addIndex':
      return createIndex(c);
    case 'editColumn': {
      const { from: a, to: b } = c;
      const name = b.name.trim();
      const type = b.type.trim();
      const def = b.default.trim();
      const renamed = name !== a.name;
      // SQLite can only rename a column.
      const retyped = e !== 'sqlite' && type !== a.type.trim();
      const nulls = e !== 'sqlite' && b.notNull !== a.notNull;
      const defaulted = e !== 'sqlite' && def !== a.default.trim();
      const setDefault = def ? `SET DEFAULT ${def}` : 'DROP DEFAULT';
      if (e === 'mysql') {
        // CHANGE restates the whole column: parts the form does not show (AUTO_INCREMENT, COMMENT, ON UPDATE) go away.
        if (retyped || nulls || (renamed && defaulted)) return `${alter}\nCHANGE COLUMN ${q(a.name)} ${q(name)} ${type}${b.notNull ? ' NOT NULL' : ' NULL'}${def ? ` DEFAULT ${def}` : ''};`;
        if (renamed) return `${alter}\nRENAME COLUMN ${q(a.name)} TO ${q(name)};`;
        return defaulted ? `${alter}\nALTER COLUMN ${q(a.name)} ${setDefault};` : '';
      }
      const parts: string[] = [];
      if (retyped) parts.push(`ALTER COLUMN ${q(a.name)} TYPE ${type}`);
      if (nulls) parts.push(`ALTER COLUMN ${q(a.name)} ${b.notNull ? 'SET' : 'DROP'} NOT NULL`);
      if (defaulted) parts.push(`ALTER COLUMN ${q(a.name)} ${setDefault}`);
      const stmts = parts.length ? [`${alter}\n${parts.join(',\n')};`] : [];
      // PG does not allow RENAME together with other actions in one ALTER TABLE.
      if (renamed) stmts.push(`${alter}\nRENAME COLUMN ${q(a.name)} TO ${q(name)};`);
      return atomic(stmts);
    }
    case 'editConstraint': {
      const { from: a } = c;
      // A MySQL primary key is always named PRIMARY.
      const pk = e === 'mysql' && a.kind === 'primary';
      const b = { ...c.to, name: pk ? '' : c.to.name.trim(), check: c.to.check.trim() };
      const body = a.kind === 'check' ? a.check.trim() === b.check : same(a.columns, b.columns);
      if (body) {
        if (pk || b.name === a.name) return '';
        if (e === 'pg') return `${alter}\nRENAME CONSTRAINT ${q(a.name)} TO ${q(b.name)};`;
        if (a.kind === 'unique') return `${alter}\nRENAME INDEX ${q(a.name)} TO ${q(b.name)};`;
      }
      return `${alter}\n${dropConstraint(a.kind, a.name)},\nADD ${constraint(b)};`;
    }
    case 'editForeignKey': {
      const { from: a } = c;
      const b = { ...c.to, name: c.to.name.trim() };
      const body = same([a.columns, a.ref.schema, a.ref.name, a.refColumns, a.onUpdate, a.onDelete], [b.columns, b.ref.schema, b.ref.name, b.refColumns, b.onUpdate, b.onDelete]);
      if (body && b.name === a.name) return '';
      if (body && e === 'pg') return `${alter}\nRENAME CONSTRAINT ${q(a.name)} TO ${q(b.name)};`;
      // MySQL refuses to drop and add one foreign key name in a single ALTER TABLE, so the new key passes
      // through a temporary name. Each statement is atomic: if one fails, a foreign key stays.
      if (e === 'mysql' && b.name === a.name) {
        const tmp = { ...b, name: `${a.name.slice(0, 56)}_sandbtmp` };
        return `${alter}\n${dropForeignKey(a.name)},\nADD ${foreignKey(tmp)};\n${alter}\n${dropForeignKey(tmp.name)},\nADD ${foreignKey(b)};`;
      }
      return `${alter}\n${dropForeignKey(a.name)},\nADD ${foreignKey(b)};`;
    }
    case 'editIndex': {
      const { from: a } = c;
      const b = { ...c.to, name: c.to.name.trim() };
      const body = same([a.columns, a.unique, a.method], [b.columns, b.unique, b.method]);
      if (body && b.name === a.name) return '';
      if (body && e === 'pg') return `ALTER INDEX ${t.schema ? q(t.schema) + '.' : ''}${q(a.name)} RENAME TO ${q(b.name)};`;
      if (body && e === 'mysql') return `${alter}\nRENAME INDEX ${q(a.name)} TO ${q(b.name)};`;
      // One MySQL ALTER TABLE drops and adds together.
      if (e === 'mysql') return `${alter}\nDROP INDEX ${q(a.name)},\nADD ${b.unique ? 'UNIQUE ' : ''}INDEX ${q(b.name)} (${list(b.columns)})${b.method ? ` USING ${b.method}` : ''};`;
      return atomic([dropIndex(a.name), createIndex(b)]);
    }
    case 'dropColumn':
      return `${alter}\nDROP COLUMN ${q(c.name)};`;
    case 'dropForeignKey':
      return `${alter}\n${dropForeignKey(c.name)};`;
    case 'dropConstraint':
      return `${alter}\n${dropConstraint(c.kind, c.name)};`;
    case 'dropIndex':
      return dropIndex(c.name);
  }
}

// ---- MongoDB: structure changes as mongosh text --------------------------------------------

/** One key of an index: a field path, or a special index type written as a string. */
export type MongoIndexField = { field: string; kind: string };

/** An index the dialog builds. `partial` is mongosh object text and is inserted verbatim. */
export type MongoIndexDraft = { name: string; fields: MongoIndexField[]; unique: boolean; sparse: boolean; hidden: boolean; ttl: string; partial: string };

/** A validator change: the full validator document as mongosh text, or `{}` to remove it. */
export type MongoValidatorDraft = { validator: string; level: string; action: string };

/** The special index types; `1` and `-1` are the ordinary ascending and descending keys. */
export const MONGO_INDEX_KINDS = ['1', '-1', 'text', '2dsphere', '2d', 'hashed'] as const;

/** The validation levels and actions `collMod` takes. */
export const MONGO_VALIDATION_LEVELS = ['strict', 'moderate', 'off'] as const;
export const MONGO_VALIDATION_ACTIONS = ['error', 'warn', 'errorAndLog'] as const;

/** One structure change of a collection. MongoDB cannot change an index's keys, so an index is dropped and added. */
export type MongoChange =
  | { op: 'createIndex'; index: MongoIndexDraft }
  | { op: 'dropIndex'; name: string }
  | { op: 'setValidator'; validator: MongoValidatorDraft }
  | { op: 'removeValidator'; validator: MongoValidatorDraft };

/** The collection, always through getCollection(): a name is then safe whatever it holds. */
export const mongoCollection = (name: string) => `db.${mongoName(name)}`;

/** The `createIndex` text of one index: keys first, options only when there are some. */
function mongoCreateIndex(coll: string, d: MongoIndexDraft): string {
  const keys = d.fields
    .filter((f) => f.field.trim())
    .map((f) => {
      const field = f.field.trim();
      return `${PLAIN_IDENT.test(field) ? field : JSON.stringify(field)}: ${f.kind === '1' || f.kind === '-1' ? f.kind : JSON.stringify(f.kind)}`;
    });
  const options = [
    d.name.trim() ? `name: ${JSON.stringify(d.name.trim())}` : '',
    d.unique ? 'unique: true' : '',
    d.sparse ? 'sparse: true' : '',
    d.hidden ? 'hidden: true' : '',
    d.ttl.trim() ? `expireAfterSeconds: ${d.ttl.trim()}` : '',
    d.partial.trim() ? `partialFilterExpression: ${d.partial.trim()}` : '',
  ].filter(Boolean);
  return `${mongoCollection(coll)}.createIndex({ ${keys.join(', ')} }${options.length ? `, { ${options.join(', ')} }` : ''});`;
}

/** Mongosh text of one structure change, for the preview and the editor run. */
export function mongoSchemaText(t: TableRef, c: MongoChange): string {
  switch (c.op) {
    case 'createIndex':
      return mongoCreateIndex(t.name, c.index);
    // A name is what dropIndex takes: the key document would not say which index.
    case 'dropIndex':
      return `${mongoCollection(t.name)}.dropIndex(${JSON.stringify(c.name)});`;
    case 'setValidator':
    case 'removeValidator': {
      const v = c.validator;
      // collMod carries the validator document itself; `{}` removes an existing one.
      const doc = c.op === 'setValidator' ? v.validator.trim() : '{}';
      const rest = [v.level ? `validationLevel: ${JSON.stringify(v.level)}` : '', v.action ? `validationAction: ${JSON.stringify(v.action)}` : ''].filter(Boolean);
      return `db.runCommand({ collMod: ${JSON.stringify(t.name)}, validator: ${doc}${rest.length ? `, ${rest.join(', ')}` : ''} });`;
    }
  }
}
