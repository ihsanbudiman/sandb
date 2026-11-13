import type { ColumnInfo, ConstraintInfo, Engine, ForeignKeyInfo, IndexInfo, TableKind } from './api';
import type { Conn } from './connections.svelte';
import { mongoValueText } from './mongo/value';
import type { SqlEngine } from './sql/text';
import { quoteIdent, sqlLiteral } from './sql/value';

/** A table, view or materialized view from the connection tree. `schema` is null on MySQL and SQLite. */
export type TableRef = { database: string; schema: string | null; name: string; kind: TableKind };

export const kindLabel: Record<TableKind, string> = { table: 'table', view: 'view', matview: 'materialized view' };

export type TableAction = 'view' | 'count' | 'ddl' | 'select' | 'insert' | 'exportDdl' | 'exportSql' | 'exportCsv' | 'exportDocuments' | 'truncate' | 'drop';

export type Danger = { action: 'truncate' | 'drop'; c: Conn; t: TableRef };

export const qualified = (e: Engine, t: TableRef) => (t.schema ? quoteIdent(e, t.schema) + '.' : '') + quoteIdent(e, t.name);

export const displayName = (t: TableRef) => (t.schema ? t.schema + '.' : '') + t.name;

export type TableFilterOperator = '=' | '<>' | '>' | '>=' | '<' | '<=' | 'contains' | 'startsWith' | 'endsWith' | 'isNull' | 'isNotNull';
export type TableFilter = { id: string; column: string; operator: TableFilterOperator; value: string };

/** One filter-row operator, with the SQL and mongosh forms it emits. `type` is the column's declared type. */
export type FilterOperator = {
  value: TableFilterOperator;
  label: string;
  /** Matches a pattern: only a text column. */
  text?: boolean;
  /** Carries no value (IS NULL / IS NOT NULL). */
  noValue?: boolean;
  /** The SQL condition; `col` is the quoted column. */
  sql: (col: string, value: string, engine: Engine, type: string) => string;
  /** The mongosh field condition; `key` is the field name as written. */
  mongo: (key: string, value: string, type: string) => string;
};

/** The pattern of a `contains`-family filter: metacharacters escaped, so the value matches literally. */
const regexText = (v: string) => v.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

const OPERATORS: Record<TableFilterOperator, FilterOperator> = {
  '=': { value: '=', label: 'equals', sql: (c, v, e, t) => `${c} = ${sqlLiteral(e, v, t)}`, mongo: (k, v, t) => `${k}: ${mongoValueText(t, v)}` },
  '<>': { value: '<>', label: 'does not equal', sql: (c, v, e, t) => `${c} <> ${sqlLiteral(e, v, t)}`, mongo: (k, v, t) => `${k}: { $ne: ${mongoValueText(t, v)} }` },
  '>': { value: '>', label: 'greater than', sql: (c, v, e, t) => `${c} > ${sqlLiteral(e, v, t)}`, mongo: (k, v, t) => `${k}: { $gt: ${mongoValueText(t, v)} }` },
  '>=': { value: '>=', label: 'at least', sql: (c, v, e, t) => `${c} >= ${sqlLiteral(e, v, t)}`, mongo: (k, v, t) => `${k}: { $gte: ${mongoValueText(t, v)} }` },
  '<': { value: '<', label: 'less than', sql: (c, v, e, t) => `${c} < ${sqlLiteral(e, v, t)}`, mongo: (k, v, t) => `${k}: { $lt: ${mongoValueText(t, v)} }` },
  '<=': { value: '<=', label: 'at most', sql: (c, v, e, t) => `${c} <= ${sqlLiteral(e, v, t)}`, mongo: (k, v, t) => `${k}: { $lte: ${mongoValueText(t, v)} }` },
  contains: { value: 'contains', label: 'contains', text: true, sql: (c, v, e) => `${c} LIKE ${sqlLiteral(e, `%${v}%`, 'text')}`, mongo: (k, v) => `${k}: { $regex: ${JSON.stringify(regexText(v))} }` },
  startsWith: { value: 'startsWith', label: 'starts with', text: true, sql: (c, v, e) => `${c} LIKE ${sqlLiteral(e, `${v}%`, 'text')}`, mongo: (k, v) => `${k}: { $regex: ${JSON.stringify(`^${regexText(v)}`)} }` },
  endsWith: { value: 'endsWith', label: 'ends with', text: true, sql: (c, v, e) => `${c} LIKE ${sqlLiteral(e, `%${v}`, 'text')}`, mongo: (k, v) => `${k}: { $regex: ${JSON.stringify(`${regexText(v)}$`)} }` },
  isNull: { value: 'isNull', label: 'is NULL', noValue: true, sql: (c) => `${c} IS NULL`, mongo: (k) => `${k}: null` },
  isNotNull: { value: 'isNotNull', label: 'is not NULL', noValue: true, sql: (c) => `${c} IS NOT NULL`, mongo: (k) => `${k}: { $ne: null }` },
};

/** The filter-row operators in menu order; OPERATORS is their only declaration. */
export const FILTER_OPERATORS = Object.values(OPERATORS);

/** One operator by its value. */
export const filterOperator = (value: TableFilterOperator) => OPERATORS[value];

/** A column whose cells are text, so a pattern operator applies. */
export const isTextType = (dataType: string) => /char|text|uuid|json|xml|citext|string/i.test(dataType || 'text');

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
  const where = filters.map((f) => filterOperator(f.operator).sql(quoteIdent(engine, f.column), f.value, engine, types[columns.indexOf(f.column)] ?? ''));
  return `SELECT *\nFROM ${qualified(engine, table)}${where.length ? `\nWHERE ${where.join(' AND ')}` : ''}`;
}

export const PLAIN_IDENT = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

/**
 * The collection access after `db.`, always through getCollection(): a bare `db.<name>` can hit a
 * database method (`db.version`), and a name may hold quotes, dots or non-ASCII text.
 */
export function mongoName(name: string) {
  return `getCollection(${JSON.stringify(name)})`;
}

function mongoCondition(f: TableFilter, type: string) {
  const key = PLAIN_IDENT.test(f.column) ? f.column : JSON.stringify(f.column);
  return filterOperator(f.operator).mongo(key, f.value, type);
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

export const COLUMN_TYPES: Record<SqlEngine, string[]> = {
  pg: ['integer', 'bigint', 'smallint', 'numeric(12,2)', 'text', 'varchar(255)', 'boolean', 'date', 'timestamp', 'timestamptz', 'uuid', 'jsonb', 'bytea'],
  mysql: ['int', 'bigint', 'smallint', 'tinyint(1)', 'decimal(12,2)', 'varchar(255)', 'text', 'date', 'datetime', 'timestamp', 'json', 'blob'],
  sqlite: ['INTEGER', 'REAL', 'TEXT', 'BLOB', 'NUMERIC'],
};

export const INDEX_METHODS: Record<SqlEngine, string[]> = {
  pg: ['btree', 'hash', 'gin', 'gist', 'brin', 'spgist'],
  mysql: ['BTREE', 'HASH'],
  sqlite: [],
};

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
  if (e === 'mysql' && def && !MYSQL_DEFAULT_SQL.test(def)) def = sqlLiteral(e, def, c.dataType);
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

/** The statements of one structure change, and what the dialog must say about them. */
export interface SchemaChangeSql {
  sql: string;
  /** The text wraps its statements in `BEGIN;` … `COMMIT;`: PostgreSQL and SQLite DDL is transactional. */
  atomic: boolean;
  /** MySQL `CHANGE COLUMN` restates the whole column, so the parts the form does not show are rewritten. */
  rewritesColumn: boolean;
}

export function schemaChangeSql(e: Engine, t: TableRef, c: SchemaChange): SchemaChangeSql {
  let atomic = false;
  let rewritesColumn = false;
  const out = (sql: string): SchemaChangeSql => ({ sql, atomic, rewritesColumn });
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
  const batch = (stmts: string[]) => {
    if (stmts.length > 1) atomic = true;
    return (stmts.length > 1 ? ['BEGIN;', ...stmts, 'COMMIT;'] : stmts).join('\n');
  };
  switch (c.op) {
    case 'addColumn':
      return out(`${alter}\nADD COLUMN ${q(c.name.trim())} ${c.type.trim()}${c.notNull ? ' NOT NULL' : ''}${c.default.trim() ? ` DEFAULT ${c.default.trim()}` : ''};`);
    case 'addConstraint':
      return out(`${alter}\nADD ${constraint(c)};`);
    case 'addForeignKey':
      return out(`${alter}\nADD ${foreignKey(c)};`);
    case 'addIndex':
      return out(createIndex(c));
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
        if (retyped || nulls || (renamed && defaulted)) {
          rewritesColumn = true;
          return out(`${alter}\nCHANGE COLUMN ${q(a.name)} ${q(name)} ${type}${b.notNull ? ' NOT NULL' : ' NULL'}${def ? ` DEFAULT ${def}` : ''};`);
        }
        if (renamed) return out(`${alter}\nRENAME COLUMN ${q(a.name)} TO ${q(name)};`);
        return out(defaulted ? `${alter}\nALTER COLUMN ${q(a.name)} ${setDefault};` : '');
      }
      const parts: string[] = [];
      if (retyped) parts.push(`ALTER COLUMN ${q(a.name)} TYPE ${type}`);
      if (nulls) parts.push(`ALTER COLUMN ${q(a.name)} ${b.notNull ? 'SET' : 'DROP'} NOT NULL`);
      if (defaulted) parts.push(`ALTER COLUMN ${q(a.name)} ${setDefault}`);
      const stmts = parts.length ? [`${alter}\n${parts.join(',\n')};`] : [];
      // PG does not allow RENAME together with other actions in one ALTER TABLE.
      if (renamed) stmts.push(`${alter}\nRENAME COLUMN ${q(a.name)} TO ${q(name)};`);
      return out(batch(stmts));
    }
    case 'editConstraint': {
      const { from: a } = c;
      // A MySQL primary key is always named PRIMARY.
      const pk = e === 'mysql' && a.kind === 'primary';
      const b = { ...c.to, name: pk ? '' : c.to.name.trim(), check: c.to.check.trim() };
      const body = a.kind === 'check' ? a.check.trim() === b.check : same(a.columns, b.columns);
      if (body) {
        if (pk || b.name === a.name) return out('');
        if (e === 'pg') return out(`${alter}\nRENAME CONSTRAINT ${q(a.name)} TO ${q(b.name)};`);
        if (a.kind === 'unique') return out(`${alter}\nRENAME INDEX ${q(a.name)} TO ${q(b.name)};`);
      }
      return out(`${alter}\n${dropConstraint(a.kind, a.name)},\nADD ${constraint(b)};`);
    }
    case 'editForeignKey': {
      const { from: a } = c;
      const b = { ...c.to, name: c.to.name.trim() };
      const body = same([a.columns, a.ref.schema, a.ref.name, a.refColumns, a.onUpdate, a.onDelete], [b.columns, b.ref.schema, b.ref.name, b.refColumns, b.onUpdate, b.onDelete]);
      if (body && b.name === a.name) return out('');
      if (body && e === 'pg') return out(`${alter}\nRENAME CONSTRAINT ${q(a.name)} TO ${q(b.name)};`);
      // MySQL refuses to drop and add one foreign key name in a single ALTER TABLE, so the new key passes
      // through a temporary name. Each statement is atomic: if one fails, a foreign key stays.
      if (e === 'mysql' && b.name === a.name) {
        const tmp = { ...b, name: `${a.name.slice(0, 56)}_sandbtmp` };
        return out(`${alter}\n${dropForeignKey(a.name)},\nADD ${foreignKey(tmp)};\n${alter}\n${dropForeignKey(tmp.name)},\nADD ${foreignKey(b)};`);
      }
      return out(`${alter}\n${dropForeignKey(a.name)},\nADD ${foreignKey(b)};`);
    }
    case 'editIndex': {
      const { from: a } = c;
      const b = { ...c.to, name: c.to.name.trim() };
      const body = same([a.columns, a.unique, a.method], [b.columns, b.unique, b.method]);
      if (body && b.name === a.name) return out('');
      if (body && e === 'pg') return out(`ALTER INDEX ${t.schema ? q(t.schema) + '.' : ''}${q(a.name)} RENAME TO ${q(b.name)};`);
      if (body && e === 'mysql') return out(`${alter}\nRENAME INDEX ${q(a.name)} TO ${q(b.name)};`);
      // One MySQL ALTER TABLE drops and adds together.
      if (e === 'mysql') return out(`${alter}\nDROP INDEX ${q(a.name)},\nADD ${b.unique ? 'UNIQUE ' : ''}INDEX ${q(b.name)} (${list(b.columns)})${b.method ? ` USING ${b.method}` : ''};`);
      return out(batch([dropIndex(a.name), createIndex(b)]));
    }
    case 'dropColumn':
      return out(`${alter}\nDROP COLUMN ${q(c.name)};`);
    case 'dropForeignKey':
      return out(`${alter}\n${dropForeignKey(c.name)};`);
    case 'dropConstraint':
      return out(`${alter}\n${dropConstraint(c.kind, c.name)};`);
    case 'dropIndex':
      return out(dropIndex(c.name));
  }
}

export type MongoIndexField = { field: string; kind: string };

/** An index the dialog builds. `partial` is mongosh object text and is inserted verbatim. */
export type MongoIndexDraft = { name: string; fields: MongoIndexField[]; unique: boolean; sparse: boolean; hidden: boolean; ttl: string; partial: string };

/** A validator change: the full validator document as mongosh text, or `{}` to remove it. */
export type MongoValidatorDraft = { validator: string; level: string; action: string };

/** The special index types; `1` and `-1` are the ordinary ascending and descending keys. */
export const MONGO_INDEX_KINDS = ['1', '-1', 'text', '2dsphere', '2d', 'hashed'] as const;

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
