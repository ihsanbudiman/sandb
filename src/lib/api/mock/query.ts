import { ApiError } from '../types';
import type { CellValue, Connection, EditableResult, RunSort, StatementResult } from '../generated/wire';
import { DEFAULT_LIMIT } from '../generated/constants';
import { colType, mockSchema, nsOf, pkOf, tableData } from './data';
import type { Row, Schema } from '../types';
import { statementKind } from './classify';

type Filter = { column: string; operator: '=' | '<>' | '>' | '>=' | '<' | '<=' | 'contains' | 'startsWith' | 'endsWith' | 'isNull' | 'isNotNull'; value: string };

function filterRows(rows: Row[], filters: Filter[]): Row[] {
  const text = (v: unknown) => (typeof v === 'object' && v !== null ? JSON.stringify(v) : String(v));
  return rows.filter((row) =>
    filters.every((f) => {
      const v = row[f.column];
      if (f.operator === 'isNull') return v == null;
      if (f.operator === 'isNotNull') return v != null;
      if (v == null) return false;
      if (f.operator === 'contains') return text(v).includes(f.value);
      if (f.operator === 'startsWith') return text(v).startsWith(f.value);
      if (f.operator === 'endsWith') return text(v).endsWith(f.value);
      const numeric = typeof v === 'number' && /^-?(?:\d+(?:\.\d*)?|\.\d+)$/.test(f.value.trim());
      const a = typeof v === 'boolean' ? v : numeric ? v : text(v);
      const b = typeof v === 'boolean' ? /^(true|1)$/i.test(f.value.trim()) : numeric ? Number(f.value) : f.value;
      if (f.operator === '=') return a === b;
      if (f.operator === '<>') return a !== b;
      if (f.operator === '>') return a > b;
      if (f.operator === '>=') return a >= b;
      if (f.operator === '<') return a < b;
      return a <= b;
    }),
  );
}

function filtersOf(sql: string): Filter[] | null {
  const input = /\bwhere\s+([\s\S]*?)(?:\blimit\b|$)/i.exec(sql)?.[1].trim().replace(/;$/, '');
  if (input === undefined) return [];
  const condition = /\s*(\w+)\s*(IS\s+NOT\s+NULL|IS\s+NULL|>=|<=|<>|=|>|<|LIKE)\s*(?:'((?:[^']|'')*)'|(-?(?:\d+(?:\.\d*)?|\.\d+)))?\s*(AND\s+|$)/gi;
  const filters: Filter[] = [];
  let at = 0;
  while (at < input.length) {
    condition.lastIndex = at;
    const m = condition.exec(input);
    if (!m || m.index !== at) return null;
    const operator = m[2].replace(/\s+/g, ' ').toUpperCase();
    const value = m[3]?.replace(/''/g, "'") ?? m[4] ?? '';
    let op: Filter['operator'];
    if (operator === 'LIKE') {
      const leading = value.startsWith('%'), trailing = value.endsWith('%');
      op = leading && trailing ? 'contains' : leading ? 'endsWith' : 'startsWith';
    } else {
      op = operator === 'IS NULL' ? 'isNull' : operator === 'IS NOT NULL' ? 'isNotNull' : (operator as Filter['operator']);
    }
    filters.push({ column: m[1], operator: op, value: operator === 'LIKE' ? value.replace(/^%|%$/g, '') : value });
    at = condition.lastIndex;
  }
  return filters;
}

export interface MockRun {
  columns: string[];
  rows: Row[];
  ms: number;
  note?: string;
  error?: string;
  table?: string;
}

export function runStatement(sql: string, schema: Schema, conn: { engine: Connection['engine']; database: string }, options: { limit?: number; sort?: RunSort } = {}): MockRun {
  const ms = 8 + Math.floor(Math.random() * 40);
  if (statementKind(sql) === 'write') return { columns: [], rows: [], ms, note: 'Statement ran (mock). 0 rows affected.' };
  const normalized = sql.replace(/"((?:[^"]|"")*)"/g, (_, name) => name.replace(/""/g, '"')).replace(/`((?:[^`]|``)*)`/g, (_, name) => name.replace(/``/g, '`'));
  const m = normalized.match(/from\s+(?:(\w+)\.)?(\w+)/i);
  const prefix = m?.[1]?.toLowerCase();
  const table = m?.[2]?.toLowerCase() ?? Object.keys(schema)[0];
  // MySQL also accepts the database name as the qualifier (`db.table`).
  const okPrefix = !prefix || prefix === nsOf(conn.engine, table) || (conn.engine === 'mysql' && prefix === conn.database.toLowerCase());
  if (!schema[table] || !okPrefix) return { columns: [], rows: [], ms, error: `relation "${prefix ? prefix + '.' : ''}${table}" does not exist` };
  const sqlLimit = Number(normalized.match(/limit\s+(\d+)/i)?.[1] ?? NaN);
  const limit = Number.isFinite(sqlLimit) ? Math.min(options.limit ?? sqlLimit, sqlLimit) : (options.limit ?? 200);
  const d = tableData(schema, table);
  const filters = filtersOf(normalized);
  const base = filters?.length ? filterRows(d.base, filters) : d.base;
  const added = filters?.length ? filterRows(d.added, filters) : d.added;
  let rows = options.sort ? [...base, ...added].map((r) => ({ ...r })) : [...base.slice(0, limit), ...added].map((r) => ({ ...r }));
  if (options.sort) {
    const { column, ascending } = options.sort;
    rows.sort((a, b) => {
      const x = a[column], y = b[column];
      if (x === y) return 0;
      if (x == null) return 1;
      if (y == null) return -1;
      const c = typeof x === 'number' && typeof y === 'number' ? x - y : String(x).localeCompare(String(y), undefined, { numeric: true });
      return ascending ? c : -c;
    });
    rows = rows.slice(0, limit);
  }
  return { columns: schema[table], rows, ms, table };
}

/** The statements of a run, as `run_query` answers them. Without `requested`, the last statement's own LIMIT above the default sizes the run, like `result::limit`. */
export function runSql(conn: Connection, sql: string, requested: number | undefined, sort?: RunSort): StatementResult[] {
  const s = mockSchema(conn);
  const parts = sql.split(';').map((p) => p.trim()).filter((p) => statementKind(p) !== 'empty');
  if (!parts.length) throw new ApiError({ code: 'invalid_input', message: 'Nothing to run' });
  if (conn.readOnly && parts.some((p) => statementKind(p) === 'write')) {
    throw new ApiError({ code: 'connection_read_only', message: `Connection "${conn.name}" is read-only: writes are refused` });
  }
  const limit = requested ?? Math.max(Number(/\blimit\s+(\d+)/i.exec(parts.at(-1)!)?.[1] ?? 0), DEFAULT_LIMIT);
  return parts.map((part, i) => {
    const r = runStatement(part, s, conn, { limit, sort });
    if (r.error) throw new ApiError({ code: 'database', message: parts.length > 1 ? `Statement ${i + 1} of ${parts.length}: ${r.error}` : r.error });
    const rows = r.rows.slice(0, limit);
    return {
      sql: part,
      columns: r.columns.map((name) => ({ name, dataType: r.table ? colType(s, r.table, name) : 'text' })),
      rows: rows.map((row) => r.columns.map((col) => row[col] as CellValue)),
      rowCount: rows.length,
      truncated: r.rows.length > rows.length,
      limit,
      affected: r.columns.length ? undefined : 0,
      isRead: statementKind(part) === 'read',
      sortable: statementKind(part) === 'read' && /^(select|with|values)\b/i.test(part.replace(/--.*$/gm, '').trim()),
      elapsedMs: r.ms,
    };
  });
}

const IDENT_SRC = '"(?:[^"]|"")*"|`(?:[^`]|``)*`|[A-Za-z_][\\w$]*';

const unquote = (part: string) =>
  part.startsWith('"') ? part.slice(1, -1).replace(/""/g, '"') : part.startsWith('`') ? part.slice(1, -1).replace(/``/g, '`') : part;

function fromTable(sql: string): string {
  const from = new RegExp(`\\bfrom\\s+((?:${IDENT_SRC})(?:\\s*\\.\\s*(?:${IDENT_SRC}))*)`, 'i').exec(sql)?.[1];
  const parts = from ? [...from.matchAll(new RegExp(IDENT_SRC, 'g'))].map((m) => unquote(m[0])) : [];
  return (parts.at(-1) ?? '').toLowerCase();
}

function selectItems(sql: string): string[] {
  const list = /select\s+([\s\S]*?)\s+from\s/i.exec(sql)?.[1]?.trim() ?? '';
  if (!list || list === '*') return [];
  const out: string[] = [];
  let depth = 0;
  let start = 0;
  let quote = '';
  for (let i = 0; i < list.length; i++) {
    const ch = list[i];
    if (quote) {
      if (ch === quote) {
        if (list[i + 1] === quote) i++;
        else quote = '';
      }
      continue;
    }
    if (ch === '"' || ch === '`' || ch === "'") quote = ch;
    else if (ch === '(') depth++;
    else if (ch === ')') depth--;
    else if (ch === ',' && depth === 0) {
      out.push(list.slice(start, i).trim());
      start = i + 1;
    }
  }
  out.push(list.slice(start).trim());
  return out.filter(Boolean);
}

function plainColumn(item: string): string | undefined {
  const m = new RegExp(`^(?:${IDENT_SRC})$`).exec(item);
  return m ? unquote(m[0]) : undefined;
}

function sourceOf(sql: string): { table: string; columns: string[]; error?: string } {
  const normalized = sql.replace(/--.*$/gm, '');
  const table = fromTable(normalized);
  const columns = selectItems(normalized).map(plainColumn).filter((c): c is string => c !== undefined);
  return { table, columns, error: table ? undefined : 'Run a SELECT to edit its rows' };
}

export function editableOf(conn: Connection, sql: string): EditableResult {
  const s = mockSchema(conn);
  const src = sourceOf(sql);
  if (src.error || !s[src.table]) return { pk: [], reason: src.error ?? `relation "${src.table}" does not exist` };
  const clean = sql.replace(/--.*$/gm, '');
  const pk = pkOf(src.table);
  let reason: string | undefined;
  const qualified = new RegExp(`\\bfrom\\s+(?:${IDENT_SRC})(?:\\s*\\.\\s*(?:${IDENT_SRC}))*(?:\\s+\\w+)?\\s*,`, 'i');
  if (/^\s*with\b/i.test(clean) || /\bunion\b/i.test(clean)) reason = 'Read-only: result is not from a single table';
  else if (/\bjoin\b/i.test(clean) || qualified.test(clean)) reason = 'Read-only: result joins several tables';
  else if (/\b(group\s+by|distinct)\b|\b(count|sum|avg|min|max)\s*\(/i.test(clean)) reason = 'Read-only: result has aggregates';
  const items = selectItems(clean);
  const names = items.map(plainColumn);
  if (!reason && names.some((n) => n === undefined)) reason = 'Read-only: result has computed columns';
  const cols = items.length ? names.filter((n): n is string => n !== undefined) : s[src.table];
  if (!reason && !pk.every((k) => cols.includes(k))) reason = `Read-only: primary key (${pk.join(', ')}) is not in the result`;
  return { table: src.table, schema: nsOf(conn.engine, src.table), pk, reason };
}

/** Applies grid operations to the mock rows; a failed operation reports its index, as the backend does. */
export function applyMockEdits(conn: Connection, sql: string, operations: { kind: 'update' | 'insert' | 'delete'; pk: Record<string, CellValue>; set: Record<string, CellValue> }[]) {
  const meta = editableOf(conn, sql);
  if (meta.reason || !meta.table) throw new ApiError({ code: 'invalid_input', message: meta.reason ?? 'Result is not editable' });
  const d = tableData(mockSchema(conn), meta.table);
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
  return {};
}
