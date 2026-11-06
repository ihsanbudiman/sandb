// Mirrors the Rust serde types in src-tauri/src/core (camelCase fields, lowercase enums).

export type Engine = 'pg' | 'mysql' | 'sqlite';
export type Env = 'production' | 'staging' | 'local';
export type SslMode = 'disable' | 'prefer' | 'require';

/** A saved connection. Never contains the password; that lives in the encrypted credentials file. */
export interface Connection {
  id: string;
  name: string;
  /** `mysql` covers MySQL and MariaDB. */
  engine: Engine;
  /** Server version from the last successful test, e.g. `PostgreSQL 16.4`; empty if never tested. */
  version: string;
  env: Env;
  host: string;
  port: number;
  /** `main` for SQLite. */
  database: string;
  user: string;
  ssl: SslMode;
  /** SQLite database file; empty for other engines. */
  file: string;
  aiWrite: boolean;
  /** Blocks every write on this connection, the user's own queries included. */
  readOnly: boolean;
}

export interface ConnectionInput {
  name: string;
  engine: Engine;
  env: Env;
  host: string;
  port: number;
  database: string;
  user: string;
  ssl: SslMode;
  file: string;
  aiWrite: boolean;
  readOnly: boolean;
  /** Version reported by a test in the dialog; null keeps the stored one. */
  version?: string | null;
}

export interface TestResult {
  serverVersion: string;
  latencyMs: number;
}

/** `matview`: a PG materialized view. */
export type TableKind = 'table' | 'view' | 'matview';

export interface TableInfo {
  name: string;
  kind: TableKind;
  /** Server statistics, not an exact count. */
  rowEstimate?: number;
  /** SQLite only: the table has an implicit rowid. */
  hasRowid?: boolean;
  /** PG only: the parent table when this table is a partition; `schema.name` when the parent is in another schema. */
  partitionOf?: string;
}

export interface ColumnInfo {
  name: string;
  dataType: string;
  nullable: boolean;
  default?: string;
  isPk: boolean;
  /** 1-based position in the primary key. */
  pkOrdinal?: number;
}

export interface IndexInfo {
  name: string;
  /** Column names in index order; expressions (PG/SQLite expression indexes) appear as their SQL text. */
  columns: string[];
  unique: boolean;
  /** The index backs the primary key. */
  primary: boolean;
  /** Access method, e.g. btree, hash, gin (PG); BTREE/FULLTEXT (MySQL); absent on SQLite. */
  method?: string;
  /** Full CREATE INDEX text when the engine gives it (PG pg_get_indexdef, SQLite sqlite_master.sql); absent otherwise. */
  definition?: string;
}

export interface ConstraintInfo {
  name: string;
  kind: 'primary' | 'unique' | 'check' | 'exclude';
  columns: string[];
  /** Check expression or PG pg_get_constraintdef text; absent when not available. */
  definition?: string;
}

export interface ForeignKeyInfo {
  name: string;
  /** Referencing side. `schema` is PG only. */
  schema?: string;
  table: string;
  columns: string[];
  /** Referenced side. `refSchema` is PG only. */
  refSchema?: string;
  refTable: string;
  refColumns: string[];
  /** e.g. 'NO ACTION', 'CASCADE', 'SET NULL', 'RESTRICT', 'SET DEFAULT' (upper case, space separated). */
  onUpdate: string;
  onDelete: string;
}

export interface TableProperties {
  indexes: IndexInfo[];
  constraints: ConstraintInfo[];
  /** Foreign keys this table declares (table = this table). */
  foreignKeys: ForeignKeyInfo[];
  /** Foreign keys in other tables that point at this table (refTable = this table). */
  references: ForeignKeyInfo[];
}

export interface SnapshotTable {
  /** PG schema; absent on MySQL and SQLite. */
  schema?: string;
  name: string;
  kind: TableKind;
  columns: { name: string; dataType: string }[];
}

export interface SchemaSnapshot {
  database: string;
  tables: SnapshotTable[];
  /** More than 2000 columns or 10000 tables: the rest are not in the snapshot. Table names past the column cap are kept, with no columns. */
  truncated: boolean;
}

/** Binary cell value; `$bytes` is base64 of at most the first 4 KiB, `len` the full size. */
export interface BytesValue {
  $bytes: string;
  len: number;
}

/** JSON as decoded by the backend: numbers, strings (bigint/numeric too), bool, null, JSON values, bytes. */
export type CellValue = string | number | boolean | null | BytesValue | CellValue[] | { [k: string]: unknown };

export interface StatementResult {
  sql: string;
  columns: { name: string; dataType: string }[];
  rows: CellValue[][];
  rowCount: number;
  /** More rows exist than the limit; they were not fetched. */
  truncated: boolean;
  /** Rows changed, for statements that return no columns. */
  affected?: number;
  /** A plain read: "Load more" and export may run it again. */
  isRead: boolean;
  /** The lone read is a query (not EXPLAIN / SHOW), so the grid may sort it server-side. */
  sortable: boolean;
  elapsedMs: number;
  /** Mock only: the table the prototype's edit mode works on. */
  table?: string;
}

export interface QueryRun {
  statements: StatementResult[];
  /** E.g. an open transaction that was rolled back at the end of the run. */
  notices?: string[];
  elapsedMs: number;
}

/** Live statement progress of a running query. */
export interface QueryProgress {
  done: number;
  total: number;
}

export interface ExportResult {
  /** The user closed the save dialog; nothing was written. */
  cancelled: boolean;
  rows: number;
  path: string;
}

export interface RunRequest {
  connId: string;
  database: string;
  sql: string;
  /** Chosen by the caller; `cancelQuery` uses it. */
  queryId: string;
  limit: number;
  /** Editor tab that runs the query. On a writable Production connection the run executes in the tab's session until commit or rollback. */
  tabId?: string;
  /** Sorts the full query result before the row limit is applied. */
  sort?: ResultSort;
}

export interface ResultSort {
  column: string;
  /** Zero-based result column index; databases sort by this ordinal. */
  columnIndex: number;
  ascending: boolean;
}

export interface EditableRequest {
  connId: string;
  database: string;
  sql: string;
}

export interface EditableResult {
  table?: string;
  schema?: string;
  pk: string[];
  reason?: string;
}

export interface EditOperation {
  key: string;
  kind: 'update' | 'insert' | 'delete';
  pk: Record<string, CellValue>;
  set: Record<string, CellValue>;
}

export interface ApplyEditsRequest extends EditableRequest {
  tabId?: string;
  operations: EditOperation[];
}

/** One cell of an editable result, found by its primary-key values. */
export interface CellValueRequest extends EditableRequest {
  tabId?: string;
  pk: Record<string, CellValue>;
  column: string;
}

export interface ApplyEditsResult {
  failed?: { index: number; message: string };
}

/** A tab's session on its connection. Only writable Production connections open one. */
export interface TabStatus {
  /** The tab has a dedicated session. */
  open: boolean;
  /** An explicit transaction is open. */
  txOpen: boolean;
  /** The transaction holds uncommitted changes. */
  pending: boolean;
  /** PG: the transaction aborted; only rollback helps. */
  aborted: boolean;
}

/** Result of the AI read guard (layer 1) for one piece of SQL. */
export interface GuardCheck {
  allowed: boolean;
  /** Why the SQL was refused, written so the model can rewrite it. */
  reason?: string;
  /** `SELECT`, `SHOW`, `DESCRIBE`, `EXPLAIN`, `PRAGMA`, or the refused statement's kind. */
  kind?: string;
}

export type WriteWarningCode =
  | 'update_without_where'
  | 'delete_without_where'
  | 'drop'
  | 'truncate'
  | 'alter_drop'
  | 'multiple_statements'
  | 'unparsable'
  | 'executable_comment';

export interface WriteInfo {
  /** `kind`: `INSERT`, `UPDATE`, `DROP TABLE`, …; `table`: the target as written. */
  statements: { kind: string; table?: string }[];
  /** `statement`: 0-based index of the statement; absent for the whole text. */
  warnings: { code: WriteWarningCode; message: string; statement?: number }[];
}

/** One AI read: at most 500 rows, stopped after 15 s, run in a read-only session. */
export interface AiReadResult {
  columns: { name: string; dataType: string }[];
  rows: CellValue[][];
  /** More rows exist than the cap; they were not fetched. */
  truncated: boolean;
  elapsedMs: number;
}

/** AI provider settings, stored in `settings.json`. The API key is stored apart, in the secret store. */
export interface AiSettings {
  provider: string;
  /** OpenAI-compatible base URL, without a trailing `/`. */
  baseUrl: string;
  model: string;
  /** `results`: query rows go to the model; `schema`: only column names and row counts. */
  privacy: 'results' | 'schema';
  maxRows: number;
  timeoutSec: number;
}

/** The saved settings. The key itself never comes back; `hasKey` tells if one is stored. */
export interface AiSettingsView extends AiSettings {
  hasKey: boolean;
}

export interface AiTestResult {
  latencyMs: number;
  /** Number of models the server lists at `GET /models`. */
  modelCount: number;
}

export interface ChatGptStatus {
  signedIn: boolean;
  accountLabel: string | null;
  usageEnabled: boolean;
}

export interface ChatGptModel {
  slug: string;
  display_name: string;
  visibility: string;
}

export interface ChatGptLogout {
  revocationUnconfirmed: boolean;
}

/** One database of a chat. */
export interface ChatSource {
  connId: string;
  /** null: the connection's own database. */
  database: string | null;
  /** PG schema that `list_tables` and `describe_table` use by default; null: `public`. */
  schema: string | null;
  /** Write mode for this database. `propose_write` also needs a connection that is not read-only. It never runs SQL. */
  allowWrites: boolean;
}

export interface ChatRequest {
  chatId: string;
  /** 1 to 8 databases. The model names them `db1`, `db2`, … in this order. */
  sources: ChatSource[];
  /** Earlier turns, oldest first, without `message`. */
  history: { role: 'user' | 'assistant'; content: string }[];
  message: string;
}

export type ChartType = 'bar' | 'line' | 'area' | 'pie' | 'scatter';
/** A chart the model asked for. `x`, `y` and `series` are columns of the result. */
export interface ChartSpec {
  type: ChartType;
  title?: string;
  x: string;
  y: string[];
  /** Splits the one `y` column into one series per distinct value. */
  series?: string;
  stacked: boolean;
}

/** One streamed step of a chat reply. Every stream ends with exactly one `done` or `error`. */
export type ChatEvent =
  | { type: 'text'; delta: string }
  | { type: 'tool_call'; id: string; name: string; args: Record<string, unknown> }
  | {
      type: 'tool_result';
      id: string;
      ok: boolean;
      /** Short text: "12 tables", "5 rows · 42 ms", or the error message. */
      summary: string;
      columns?: string[];
      /** At most 50 rows, for this UI only. */
      rows?: CellValue[][];
      truncated?: boolean;
      error?: AppErrorBody;
    }
  /** Sent before the matching `tool_result` when the proposed write is allowed. */
  | { type: 'write_preview'; id: string; sql: string; info: WriteInfo }
  /** Sent before the matching `tool_result` of a `render_chart` call. At most 1000 rows, for this UI only. */
  | { type: 'chart'; id: string; spec: ChartSpec; columns: string[]; rows: CellValue[][]; truncated: boolean }
  | { type: 'error'; error: AppErrorBody }
  | { type: 'done' };

export type ErrorCode =
  | 'not_found'
  | 'invalid_input'
  | 'auth_failed'
  | 'connect_failed'
  | 'timeout'
  | 'secret_store'
  | 'io'
  | 'database'
  /** The user cancelled a running query. */
  | 'cancelled'
  /** The AI read guard refused the SQL before it reached the database. */
  | 'guard_rejected'
  /** The database refused a write inside a read-only AI read. */
  | 'read_only_violation'
  /** A write was refused: the connection is read-only (HTTP 403 in the bridge). */
  | 'connection_read_only'
  | 'internal'
  /** Dev bridge only: missing or wrong bearer token. */
  | 'unauthorized'
  /** The AI server does not list the model in the settings (HTTP 422 in the bridge). */
  | 'model_not_found'
  /** The AI server answered with an error status; `detail` holds the start of the body (HTTP 502 in the bridge). */
  | 'ai_upstream'
  /** The AI server sent a reply that is not valid, or the reply used too many steps (HTTP 502 in the bridge). */
  | 'ai_protocol'
  /** `propose_write` was refused: write mode is off or the connection is read-only (HTTP 403 in the bridge). */
  | 'write_blocked';

export interface AppErrorBody {
  code: ErrorCode;
  message: string;
  detail?: string;
  http_status?: number;
  request_id?: string;
  upstream_code?: string;
  upstream_param?: string;
  upstream_body?: unknown;
}

export class ApiError extends Error {
  code: ErrorCode;
  detail?: string;
  http_status?: number;
  request_id?: string;
  upstream_code?: string;
  upstream_param?: string;
  upstream_body?: unknown;

  constructor(body: AppErrorBody) {
    super(body.message);
    this.name = 'ApiError';
    this.code = body.code;
    this.detail = body.detail;
    this.http_status = body.http_status;
    this.request_id = body.request_id;
    this.upstream_code = body.upstream_code;
    this.upstream_param = body.upstream_param;
    this.upstream_body = body.upstream_body;
  }

  static from(e: unknown): ApiError {
    if (e instanceof ApiError) return e;
    if (e && typeof e === 'object' && 'code' in e && 'message' in e) return new ApiError(e as AppErrorBody);
    return new ApiError({ code: 'internal', message: e instanceof Error ? e.message : String(e) });
  }
}

/**
 * The commands every transport implements. `password`: null keeps the stored secret
 * (update/duplicate) or means none (create); an empty string clears it.
 */
export interface Backend {
  kind: 'tauri' | 'bridge' | 'mock';
  listConnections(): Promise<Connection[]>;
  createConnection(input: ConnectionInput, password: string | null): Promise<Connection>;
  updateConnection(id: string, input: ConnectionInput, password: string | null): Promise<Connection>;
  /** `input` overrides the copied fields; `password: null` copies the source secret. */
  duplicateConnection(id: string, input: ConnectionInput | null, password: string | null): Promise<Connection>;
  deleteConnection(id: string): Promise<void>;
  /** Tests `input`. Without a password, the stored secret of `id` is used. */
  testConnection(input: ConnectionInput, password: string | null, id: string | null): Promise<TestResult>;
  listDatabases(connId: string): Promise<string[]>;
  /** PG only; empty for MySQL and SQLite (no schema level). */
  listSchemas(connId: string, database: string): Promise<string[]>;
  listTables(connId: string, database: string, schema: string | null): Promise<TableInfo[]>;
  listColumns(connId: string, database: string, schema: string | null, table: string): Promise<ColumnInfo[]>;
  /** Indexes, constraints, foreign keys and incoming references of one table. A view yields empty lists; an unknown table fails with `not_found`. */
  tableProperties(connId: string, database: string, schema: string | null, table: string): Promise<TableProperties>;
  schemaSnapshot(connId: string, database: string): Promise<SchemaSnapshot>;
  /** Runs every statement in `sql` on one connection; fails with code `cancelled` after `cancelQuery`. */
  runQuery(request: RunRequest): Promise<QueryRun>;
  /** Resolves the source table and primary key of a simple single-table SELECT. */
  editableResult(request: EditableRequest): Promise<EditableResult>;
  /** Applies all grid operations atomically; a database failure identifies its operation. */
  applyEdits(request: ApplyEditsRequest): Promise<ApplyEditsResult>;
  /** The full text of a cell that the grid shows cut off (PostGIS geometry as WKT); `null` for NULL. */
  cellValue(request: CellValueRequest): Promise<string | null>;
  cancelQuery(queryId: string): Promise<void>;
  /** `done`/`total` while the run is in progress; `null` for an unknown or finished id. */
  queryProgress(queryId: string): Promise<QueryProgress | null>;
  /** Commits the tab's open transaction. Fails with `not_found` when the tab has no session. */
  commitTransaction(tabId: string): Promise<void>;
  /** Rolls back the tab's open transaction; the session stays open. */
  rollbackTransaction(tabId: string): Promise<void>;
  /** `null` when the tab has no session. */
  tabStatus(tabId: string): Promise<TabStatus | null>;
  /** Drops the tab's session, rolling back anything uncommitted. */
  closeTabSession(tabId: string): Promise<void>;
  /** Closes the app. Pending transactions are rolled back, so commit or roll back each tab first. */
  quitApp(): Promise<void>;
  /** Tauri only: asks for a file in a native save dialog and streams the full result of one read to it. Other transports export in the browser. `tabId` reads inside the tab's open transaction, as `runQuery` does. */
  exportCsv(connId: string, database: string, sql: string, tabId?: string): Promise<ExportResult>;
  /** `CREATE` statement(s) of one table or view: the table, then its indexes (and triggers on SQLite). */
  tableDdl(connId: string, database: string, schema: string | null, table: string): Promise<string>;
  /** Tauri only: asks for a file in a native save dialog and writes the table's DDL to it; with `data`, INSERT statements for every row follow. */
  exportTableSql(connId: string, database: string, schema: string | null, table: string, data: boolean): Promise<ExportResult>;
  /** Tauri only: asks for a file in a native save dialog and writes the PDF to it. `name` is the suggested ASCII file name. Resolves to the path, or null when the user closed the dialog. Other transports download in the browser. */
  savePdf(name: string, pdf: Uint8Array): Promise<string | null>;
  /** Checks SQL against the AI read guard without running it. */
  guardCheck(engine: Engine, sql: string): Promise<GuardCheck>;
  /** Describes a proposed write for the approval preview. Never runs it. */
  classifyWrite(engine: Engine, sql: string): Promise<WriteInfo>;
  /** AI read path: guard, then a read-only session with the default limits. Fails with `guard_rejected`, `read_only_violation` or `timeout`. */
  runAiRead(connId: string, database: string | null, sql: string): Promise<AiReadResult>;
  chatgptStatus(): Promise<ChatGptStatus>;
  chatgptSignIn(enableUsage: boolean): Promise<ChatGptStatus>;
  chatgptCancelSignIn(): Promise<void>;
  chatgptLogout(): Promise<ChatGptLogout>;
  chatgptModels(): Promise<ChatGptModel[]>;
  /** Native only: opens the fixed ChatGPT usage settings page. */
  chatgptManageUsage(): Promise<void>;
  getAiSettings(): Promise<AiSettingsView>;
  /** Fails with `invalid_input` for a URL that is not http(s), max rows outside 1–10,000 or a time limit outside 1–300 s. */
  saveAiSettings(settings: AiSettings): Promise<AiSettingsView>;
  /** null or an empty string deletes the stored key. */
  setAiKey(key: string | null): Promise<void>;
  /** Lists the models of `settings` (maybe not saved) and fails with `model_not_found` when `settings.model` is not among them. `key`: null uses the stored key; an empty string sends none. */
  testAi(settings: AiSettings, key: string | null): Promise<AiTestResult>;
  /** Model ids of `settings` (maybe not saved), sorted. `settings.model` is not checked. `key` as in `testAi`. */
  listAiModels(settings: AiSettings, key: string | null): Promise<string[]>;
  /** Streams one reply to `onEvent` and resolves after the terminal `done` or `error` event. */
  chatSend(request: ChatRequest, onEvent: (e: ChatEvent) => void): Promise<void>;
  /** Stops a running reply; it then ends with an `error` event of code `cancelled`. An unknown id is not an error. */
  chatCancel(chatId: string): Promise<void>;
}
