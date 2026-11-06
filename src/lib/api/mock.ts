import { ApiError, type Backend, type Connection, type ConnectionInput, type Engine, type StatementResult, type CellValue, type GuardCheck, type WriteInfo, type ForeignKeyInfo, type TableProperties, type AiSettings, type EditableResult } from './types';
import { schemas, nsOf, pkOf, colType, runMock, statementKind, tableData } from '../mock';

// In-memory backend: the prototype behaviour, used when there is neither Tauri nor the dev bridge.

const versions: Record<Engine, string> = { pg: 'PostgreSQL 16.4', mysql: 'MySQL 8.4', sqlite: 'SQLite 3.46' };

export const seedConnections: Connection[] = [
  { id: 'prod', name: 'prod-analytics', engine: 'pg', version: 'PostgreSQL 16.4', env: 'production', host: 'prod-analytics.db.internal', port: 5432, database: 'analytics_prod', user: 'analyst', ssl: 'require', file: '', aiWrite: false, readOnly: false },
  { id: 'shop', name: 'shop-staging', engine: 'mysql', version: 'MySQL 8.4', env: 'staging', host: 'shop-staging.db.internal', port: 3306, database: 'shop', user: 'app', ssl: 'prefer', file: '', aiWrite: false, readOnly: false },
  { id: 'notes', name: 'notes.db', engine: 'sqlite', version: 'SQLite 3.46', env: 'local', host: '', port: 0, database: 'main', user: '', ssl: 'prefer', file: '~/data/notes.db', aiWrite: false, readOnly: false },
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

function make(id: string, i: ConnectionInput, version: string): Connection {
  const sqlite = i.engine === 'sqlite';
  return {
    id,
    name: i.name.trim(),
    engine: i.engine,
    version,
    env: i.env,
    host: sqlite ? '' : i.host.trim(),
    port: sqlite ? 0 : i.port,
    database: sqlite ? 'main' : i.database.trim(),
    user: sqlite ? '' : i.user.trim(),
    ssl: i.ssl,
    file: sqlite ? i.file.trim() : '',
    aiWrite: i.aiWrite,
    readOnly: i.readOnly,
  };
}

// Same mapping as the prototype: each engine shows one of the mock schemas.
const mockSchemaOf: Record<Engine, string> = { pg: 'prod', mysql: 'shop', sqlite: 'notes' };
const mockSchema = (c: Connection) => schemas[mockSchemaOf[c.engine]];

function find(id: string) {
  const c = list.find((x) => x.id === id);
  if (!c) throw new ApiError({ code: 'not_found', message: `Connection ${id} not found` });
  return c;
}

// Keyword stand-ins for the Rust guard; the mock has no parser. Never a security boundary.
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
const noChatGpt = () => new ApiError({ code: 'invalid_input', message: 'ChatGPT sign-in and plan usage require the native desktop app.' });
const noAi = () => new ApiError({ code: 'invalid_input', message: 'The demo build has no AI backend. Use the desktop app or the dev bridge.' });

export const mockBackend: Backend = {
  kind: 'mock',
  async listConnections() {
    return list.map((c) => ({ ...c }));
  },
  async createConnection(input) {
    const c = make(newId(input.name), input, input.version || versions[input.engine]);
    list.push(c);
    return { ...c };
  },
  // Saving or deleting a connection drops its tab sessions, as the backend does.
  async updateConnection(id, input) {
    const cur = find(id);
    const c = make(id, input, input.version || cur.version);
    list = list.map((x) => (x.id === id ? c : x));
    dropSessionsOf(id);
    return { ...c };
  },
  async duplicateConnection(id, input) {
    const src = find(id);
    const i: ConnectionInput = input ?? { ...src, name: `${src.name} (copy)`, aiWrite: false };
    const c = make(newId(i.name), i, src.version);
    list.push(c);
    return { ...c };
  },
  async deleteConnection(id) {
    find(id);
    list = list.filter((x) => x.id !== id);
    dropSessionsOf(id);
  },
  async testConnection(input) {
    await sleep(900);
    return { serverVersion: versions[input.engine], latencyMs: 12 };
  },
  // Schema browsing and queries run on the prototype data in ../mock.ts.
  async listDatabases(connId) {
    return [find(connId).database];
  },
  async listSchemas(connId) {
    const c = find(connId);
    if (c.engine !== 'pg') return [];
    return [...new Set(Object.keys(mockSchema(c)).map((t) => nsOf(c.engine, t)))];
  },
  async listTables(connId, _database, schema) {
    const c = find(connId);
    return Object.keys(mockSchema(c))
      .filter((t) => c.engine !== 'pg' || nsOf(c.engine, t) === (schema ?? 'public'))
      .map((name) => ({ name, kind: 'table' as const }));
  },
  async listColumns(connId, _database, _schema, table) {
    const s = mockSchema(find(connId));
    if (!s[table]) throw new ApiError({ code: 'not_found', message: `Table ${table} not found` });
    const pk = pkOf(table);
    return s[table].map((name) => ({ name, dataType: colType(s, table, name), nullable: !pk.includes(name), isPk: pk.includes(name), pkOrdinal: pk.includes(name) ? pk.indexOf(name) + 1 : undefined }));
  },
  async tableProperties(connId, _database, _schema, table) {
    const c = find(connId);
    if (!mockSchema(c)[table]) throw new ApiError({ code: 'not_found', message: `Table ${table} not found` });
    return mockProperties(c, table);
  },
  async schemaSnapshot(connId, database) {
    const c = find(connId);
    const s = mockSchema(c);
    return {
      database,
      truncated: false,
      tables: Object.entries(s).map(([name, cols]) => ({ schema: c.engine === 'pg' ? nsOf(c.engine, name) : undefined, name, kind: 'table' as const, columns: cols.map((col) => ({ name: col, dataType: colType(s, name, col) })) })),
    };
  },
  async runQuery({ connId, sql, limit, tabId, sort }) {
    const c = find(connId);
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
    return mockEditable(find(connId), sql);
  },
  async applyEdits({ connId, sql, tabId, operations }) {
    const c = find(connId);
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
    const meta = mockEditable(find(connId), sql);
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
  // The prototype data has types and primary keys only, so the DDL has no defaults or other constraints.
  async tableDdl(connId, _database, schema, table) {
    const c = find(connId);
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
  async guardCheck(_engine, sql) {
    return mockGuard(sql);
  },
  async classifyWrite(_engine, sql) {
    return mockClassifyWrite(sql);
  },
  async runAiRead(connId, database, sql) {
    const g = mockGuard(sql);
    if (!g.allowed) throw new ApiError({ code: 'guard_rejected', message: `Blocked by the read-only guard: ${g.reason}` });
    const run = await mockBackend.runQuery({ connId, database: database ?? '', sql, queryId: 'ai', limit: 500 });
    const s = run.statements[0];
    return { columns: s.columns, rows: s.rows, truncated: s.truncated, elapsedMs: s.elapsedMs };
  },
  async chatgptStatus() { return { signedIn: false, accountLabel: null, usageEnabled: false }; },
  async chatgptSignIn() { throw noChatGpt(); },
  async chatgptCancelSignIn() { throw noChatGpt(); },
  async chatgptLogout() { throw noChatGpt(); },
  async chatgptModels() { throw noChatGpt(); },
  async chatgptManageUsage() { throw noChatGpt(); },
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
};
