import { ApiError } from '../types';
import type {
  AiReadResult,
  ApplyEditsArgs,
  ApproveWriteArgs,
  CellValueArgs,
  ChatCancelArgs,
  ChatEvent,
  ChatRequest,
  ClassifyWriteArgs,
  CloseTabSessionArgs,
  CommitTransactionArgs,
  CommandName,
  Connection,
  ConnectionInput,
  CreateConnectionArgs,
  DeleteConnectionArgs,
  DenyWriteArgs,
  DuplicateConnectionArgs,
  EditableResultArgs,
  Engine,
  ForeignKeyInfo,
  GuardCheckArgs,
  ListColumnsArgs,
  ListDatabasesArgs,
  ListSchemasArgs,
  ListTablesArgs,
  ParseMongoUriArgs,
  ReorderConnectionsArgs,
  RollbackTransactionArgs,
  RunAiReadArgs,
  RunQueryArgs,
  SaveAiSettingsArgs,
  SchemaSnapshot,
  SchemaSnapshotArgs,
  SetAiKeyArgs,
  TabStatusArgs,
  TableDdlArgs,
  TableProperties,
  TablePropertiesArgs,
  TestConnectionArgs,
  Topology,
  UpdateConnectionArgs,
} from '../generated/wire';
import { MAX_ROWS_LIMIT, TIMEOUT_SEC_LIMIT } from '../generated/constants';
import { colType, mockSchema, mongoCollections, mongoSchema, nsOf, pkOf, tableData, versions } from './data';
import { guard, classify, statementKind } from './classify';
import { mongoProperties, mongoRun, mongoScript } from './mongo';
import { applyMockEdits, editableOf, runSql } from './query';
import { parseMongoUri, redactMongoUri } from './uri';
import { ai, all, add, find, remove, reorder, replace, sessions } from './state';
import { chatCancel, chatStream, decideWrite } from './chat';

export type Emit = (e: ChatEvent) => void;
/** The demo's answer to one command, with the arguments of that command; the table only names the commands. */
export type Handler = (args: never, emit: Emit) => Promise<unknown>;

const DEFAULT_PORTS: Record<Engine, number> = { pg: 5432, mysql: 3306, sqlite: 0, mongo: 27017 };
const invalid = (message: string, detail?: string) => new ApiError({ code: 'invalid_input', message, ...(detail ? { detail } : {}) });

const newId = (name: string) => name.trim().toLowerCase().replace(/[^a-z0-9]+/g, '-') + '-' + Date.now().toString(36);

/** Trims the fields and applies the same rules as the backend's `normalize`. */
function normalize(input: ConnectionInput): Required<ConnectionInput> {
  const i: Required<ConnectionInput> = {
    name: input.name.trim(),
    engine: input.engine,
    env: input.env ?? 'local',
    host: (input.host ?? '').trim(),
    port: input.port ?? 0,
    database: (input.database ?? '').trim(),
    user: (input.user ?? '').trim(),
    ssl: input.ssl ?? 'prefer',
    file: (input.file ?? '').trim(),
    aiWrite: input.aiWrite ?? false,
    readOnly: input.readOnly ?? false,
    uri: (input.uri ?? '').trim(),
    authSource: (input.authSource ?? '').trim(),
    topology: input.topology ?? null,
    version: input.version ?? null,
  };
  if (i.readOnly) i.aiWrite = false;
  if (i.engine === 'sqlite') {
    if (!i.file) throw invalid('Database file is required');
    i.database = 'main';
    i.host = '';
    i.user = '';
    i.port = 0;
  } else if (i.engine === 'pg' || i.engine === 'mysql') {
    if (!i.host) throw invalid('Host is required');
    if (!i.database) throw invalid('Database is required');
    if (!i.user) throw invalid('User is required');
    if (!i.port) i.port = DEFAULT_PORTS[i.engine];
    i.file = '';
  } else {
    i.file = '';
    if (i.uri) {
      // The demo has no driver: the string is kept without its password, and the fields stay as typed.
      const redacted = redactMongoUri(i.uri);
      if (redacted === null) throw invalid('Invalid connection string', 'It must start with mongodb:// or mongodb+srv://');
      i.uri = redacted;
    } else {
      if (!i.host) throw invalid('Host is required');
      if (i.ssl === 'prefer') throw invalid('MongoDB has no Prefer TLS mode. Choose Disable or Require.');
    }
    if (!i.port) i.port = DEFAULT_PORTS.mongo;
  }
  if (i.engine !== 'mongo') {
    i.uri = '';
    i.authSource = '';
    i.topology = null;
  }
  return i;
}

function build(id: string, i: Required<ConnectionInput>, prevVersion: string, prevTopology: Topology | null): Connection {
  return {
    id,
    name: i.name,
    engine: i.engine,
    version: i.version ?? prevVersion,
    env: i.env,
    host: i.host,
    port: i.port,
    database: i.database,
    user: i.user,
    ssl: i.ssl,
    file: i.file,
    aiWrite: i.aiWrite,
    readOnly: i.readOnly,
    uri: i.uri ?? '',
    authSource: i.authSource ?? '',
    topology: i.engine === 'mongo' ? (i.topology ?? prevTopology) : null,
  };
}

/** `find` for the demo's SQL-only edit paths: the fake has no Mongo edit engine (the real backend edits Mongo cells). */
function findSql(id: string): Connection {
  const c = find(id);
  if (c.engine === 'mongo') throw new ApiError({ code: 'unsupported', message: 'The demo build cannot edit MongoDB collections' });
  return c;
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

export async function listConnections() {
  return all().map((c) => ({ ...c }));
}

export async function createConnection(args: CreateConnectionArgs) {
  const i = normalize(args.input);
  if (!i.name) throw invalid('Name is required');
  const c = build(newId(i.name), i, versions[i.engine], i.engine === 'mongo' ? 'replica_set' : null);
  add(c);
  return { ...c };
}

export async function updateConnection(args: UpdateConnectionArgs) {
  const cur = find(args.id);
  const i = normalize(args.input);
  if (!i.name) throw invalid('Name is required');
  const c = build(cur.id, i, cur.version, cur.topology);
  replace(c);
  return { ...c };
}

export async function duplicateConnection(args: DuplicateConnectionArgs) {
  const src = find(args.id);
  const i = normalize(args.input ?? { ...src, name: `${src.name} (copy)`, aiWrite: false });
  if (!i.name) throw invalid('Name is required');
  const c = build(newId(i.name), i, src.version, src.topology);
  add(c);
  return { ...c };
}

export async function deleteConnection(args: DeleteConnectionArgs) {
  find(args.id);
  remove(args.id);
}

export async function reorderConnections(args: ReorderConnectionsArgs) {
  reorder(args.ids);
  return all().map((c) => ({ ...c }));
}

export async function testConnection(args: TestConnectionArgs) {
  const i = normalize(args.input);
  return { serverVersion: versions[i.engine], latencyMs: 12, ...(i.engine === 'mongo' ? { topology: 'replica_set' as const } : {}) };
}

export async function listDatabases(args: ListDatabasesArgs) {
  const c = find(args.connId);
  if (c.engine === 'mongo') return ['analytics', 'shop_staging'];
  return [c.database];
}

export async function listSchemas(args: ListSchemasArgs) {
  const c = find(args.connId);
  if (c.engine !== 'pg') return [];
  return [...new Set(Object.keys(mockSchema(c)).map((t) => nsOf(c.engine, t)))];
}

export async function listTables(args: ListTablesArgs) {
  const c = find(args.connId);
  if (c.engine === 'mongo') return mongoCollections[args.database] ?? [];
  const schema = args.schema;
  return Object.keys(mockSchema(c))
    .filter((t) => c.engine !== 'pg' || nsOf(c.engine, t) === (schema ?? 'public'))
    .map((name) => ({ name, kind: 'table' as const }));
}

export async function listColumns(args: ListColumnsArgs) {
  const c = find(args.connId);
  const table = args.table;
  if (c.engine === 'mongo') {
    const cols = mongoSchema[table];
    if (!cols) throw new ApiError({ code: 'not_found', message: `Collection ${table} not found` });
    return cols;
  }
  const s = mockSchema(c);
  if (!s[table]) throw new ApiError({ code: 'not_found', message: `Table ${table} not found` });
  const pk = pkOf(table);
  return s[table].map((name) => ({ name, dataType: colType(s, table, name), nullable: !pk.includes(name), isPk: pk.includes(name), pkOrdinal: pk.includes(name) ? pk.indexOf(name) + 1 : undefined }));
}

export async function tableProperties(args: TablePropertiesArgs) {
  const c = find(args.connId);
  const table = args.table;
  if (c.engine === 'mongo') return mongoProperties(args.database, table);
  const s = mockSchema(c);
  if (!s[table]) throw new ApiError({ code: 'not_found', message: `Table ${table} not found` });
  return mockProperties(c, table);
}

export async function schemaSnapshot(args: SchemaSnapshotArgs) {
  const c = find(args.connId);
  const database = args.database;
  if (c.engine === 'mongo') {
    const tables = mongoCollections[database] ?? [];
    return { database, truncated: false, tables: tables.map((t) => ({ name: t.name, kind: t.kind, columns: (mongoSchema[t.name] ?? []).map((col) => ({ name: col.name, dataType: col.dataType })) })) } satisfies SchemaSnapshot;
  }
  const s = mockSchema(c);
  return {
    database,
    truncated: false,
    tables: Object.entries(s).map(([name, cols]) => ({ schema: c.engine === 'pg' ? nsOf(c.engine, name) : undefined, name, kind: 'table' as const, columns: cols.map((col) => ({ name: col, dataType: colType(s, name, col) })) })),
  } satisfies SchemaSnapshot;
}

export async function parseMongoUriCommand(args: ParseMongoUriArgs) {
  return parseMongoUri(args.uri);
}

export async function runQuery(args: RunQueryArgs) {
  const request = args.request;
  const c = find(request.connId);
  const limit = request.limit ?? undefined;
  if (c.engine === 'mongo') return mongoRun(c, request.sql, limit, request.sort ?? undefined);
  const statements = runSql(c, request.sql, limit, request.sort ?? undefined);
  // A tab keeps a session only while its transaction holds changes (a typed COMMIT / ROLLBACK ends it).
  if (request.tabId && c.env === 'production' && !c.readOnly) {
    let pending = sessions.has(request.tabId);
    for (const part of request.sql.split(';')) {
      if (/^\s*(commit|rollback|end)\b/i.test(part)) pending = false;
      else if (statementKind(part) === 'write') pending = true;
    }
    if (pending) sessions.set(request.tabId, c.id);
    else sessions.delete(request.tabId);
  }
  return { statements, elapsedMs: statements.reduce((n, s) => n + s.elapsedMs, 0) };
}

export async function editableResult(args: EditableResultArgs) {
  const request = args.request;
  const c = find(request.connId);
  if (c.engine === 'mongo') return { pk: [], reason: 'MongoDB edits need a live connection' };
  return editableOf(c, request.sql);
}

export async function applyEdits(args: ApplyEditsArgs) {
  const request = args.request;
  const c = findSql(request.connId);
  if (c.readOnly) throw new ApiError({ code: 'connection_read_only', message: `Connection "${c.name}" is read-only: writes are refused` });
  const out = applyMockEdits(c, request.sql, request.operations);
  if (!out.failed && request.tabId && c.env === 'production') sessions.set(request.tabId, c.id);
  return out;
}

export async function cellValue(args: CellValueArgs) {
  const request = args.request;
  const c = findSql(request.connId);
  const meta = editableOf(c, request.sql);
  if (meta.reason || !meta.table) throw invalid(meta.reason ?? 'Result is not editable');
  const d = tableData(mockSchema(c), meta.table);
  const row = [...d.base, ...d.added].find((r) => meta.pk.every((k) => r[k] === request.pk[k]));
  if (!row) throw new ApiError({ code: 'not_found', message: 'The row no longer exists or its primary key changed' });
  const v = row[request.column];
  return v === null || v === undefined ? null : typeof v === 'object' ? JSON.stringify(v) : String(v);
}

export async function cancelQuery() {}

export async function queryProgress() {
  return null;
}

export async function commitTransaction(args: CommitTransactionArgs) {
  if (!sessions.delete(args.tabId)) throw new ApiError({ code: 'not_found', message: 'No session for this tab' });
}

export async function rollbackTransaction(args: RollbackTransactionArgs) {
  if (!sessions.delete(args.tabId)) throw new ApiError({ code: 'not_found', message: 'No session for this tab' });
}

export async function tabStatus(args: TabStatusArgs) {
  return sessions.has(args.tabId) ? { open: true, txOpen: true, pending: true, aborted: false } : null;
}

export async function closeTabSession(args: CloseTabSessionArgs) {
  sessions.delete(args.tabId);
}

export async function tableDdl(args: TableDdlArgs) {
  const c = find(args.connId);
  const table = args.table;
  const database = args.database;
  if (c.engine === 'mongo') return mongoScript(database, table);
  const s = mockSchema(c);
  if (!s[table]) throw new ApiError({ code: 'not_found', message: `Table ${table} not found` });
  const pk = pkOf(table);
  const lines = s[table].map((col) => `  ${col} ${colType(s, table, col)}${pk.includes(col) ? ' NOT NULL' : ''}`);
  if (pk.length) lines.push(`  PRIMARY KEY (${pk.join(', ')})`);
  return `CREATE TABLE ${args.schema ? args.schema + '.' : ''}${table} (\n${lines.join(',\n')}\n);`;
}

export async function guardCheck(args: GuardCheckArgs) {
  return guard(args.engine, args.sql);
}

export async function classifyWrite(args: ClassifyWriteArgs) {
  return classify(args.engine, args.sql, args.database);
}

export async function runAiRead(args: RunAiReadArgs) {
  const connId = args.connId;
  const sql = args.sql;
  const c = find(connId);
  const g = guard(c.engine, sql);
  if (!g.allowed) throw new ApiError({ code: 'guard_rejected', message: `Blocked by the read-only guard: ${g.reason}` });
  const run = c.engine === 'mongo' ? mongoRun(c, sql, ai.settings.maxRows) : { statements: runSql(c, sql, ai.settings.maxRows), elapsedMs: 0 };
  const s = run.statements[0];
  const out: AiReadResult = { columns: s.columns, rows: s.rows, truncated: s.truncated, elapsedMs: s.elapsedMs };
  if (s.documents) out.documents = s.documents;
  return out;
}

export async function getAiSettings() {
  return { ...ai.settings, hasKey: !!ai.key };
}

export async function saveAiSettings(args: SaveAiSettingsArgs) {
  const s = args.settings;
  const baseUrl = s.baseUrl.trim().replace(/\/+$/, '');
  if (!/^https?:\/\//.test(baseUrl)) throw invalid('The base URL must start with http:// or https://');
  if (!(s.maxRows >= 1 && s.maxRows <= MAX_ROWS_LIMIT)) throw invalid(`Max rows must be between 1 and ${MAX_ROWS_LIMIT}`);
  if (!(s.timeoutSec >= 1 && s.timeoutSec <= TIMEOUT_SEC_LIMIT)) throw invalid(`The time limit must be between 1 and ${TIMEOUT_SEC_LIMIT} seconds`);
  ai.settings = { ...s, baseUrl, model: s.model.trim() };
  return { ...ai.settings, hasKey: !!ai.key };
}

export async function setAiKey(args: SetAiKeyArgs) {
  ai.key = args.key ?? '';
}

export async function chatSend(args: { request: ChatRequest }, emit: Emit) {
  await chatStream(args.request, emit);
}

export async function chatCancelCommand(args: ChatCancelArgs) {
  chatCancel(args.chatId);
}

export async function approveWrite(args: ApproveWriteArgs) {
  decideWrite(args.chatId, args.id, true);
}

export async function denyWrite(args: DenyWriteArgs) {
  decideWrite(args.chatId, args.id, false);
}

export const handlers: Partial<Record<CommandName, Handler>> = {
  list_connections: listConnections,
  create_connection: createConnection,
  update_connection: updateConnection,
  duplicate_connection: duplicateConnection,
  delete_connection: deleteConnection,
  reorder_connections: reorderConnections,
  test_connection: testConnection,
  list_databases: listDatabases,
  list_schemas: listSchemas,
  list_tables: listTables,
  list_columns: listColumns,
  table_properties: tableProperties,
  schema_snapshot: schemaSnapshot,
  parse_mongo_uri: parseMongoUriCommand,
  run_query: runQuery,
  editable_result: editableResult,
  apply_edits: applyEdits,
  cell_value: cellValue,
  cancel_query: cancelQuery,
  query_progress: queryProgress,
  commit_transaction: commitTransaction,
  rollback_transaction: rollbackTransaction,
  tab_status: tabStatus,
  close_tab_session: closeTabSession,
  table_ddl: tableDdl,
  guard_check: guardCheck,
  classify_write: classifyWrite,
  run_ai_read: runAiRead,
  get_ai_settings: getAiSettings,
  save_ai_settings: saveAiSettings,
  set_ai_key: setAiKey,
  chat_send: chatSend,
  chat_cancel: chatCancelCommand,
  approve_write: approveWrite,
  deny_write: denyWrite,
};
