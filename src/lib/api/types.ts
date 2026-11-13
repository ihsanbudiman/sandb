// UI-only additions. Everything that crosses the wire is generated in ./generated/wire.
import type {
  AiReadResult,
  AiSettingsView,
  AiTestResult,
  AppError,
  ApplyEditsArgs,
  ApplyEditsResult,
  ApproveWriteArgs,
  CancelQueryArgs,
  CellValue,
  CellValueArgs,
  ChatCancelArgs,
  ChatEvent,
  ChatRequest,
  ClassifyWriteArgs,
  CloseTabSessionArgs,
  ColumnInfo,
  CommitTransactionArgs,
  Connection,
  CreateConnectionArgs,
  DeleteConnectionArgs,
  DenyWriteArgs,
  DocumentExportRequest,
  DocumentFormat,
  DuplicateConnectionArgs,
  EditableResult,
  EditableResultArgs,
  ErrorCode,
  ExportRequest,
  ExportResult,
  GuardCheck,
  GuardCheckArgs,
  ListAiModelsArgs,
  ListColumnsArgs,
  ListDatabasesArgs,
  ListSchemasArgs,
  ListTablesArgs,
  MongoUriParts,
  ParseMongoUriArgs,
  QueryProgress,
  QueryProgressArgs,
  QueryRun,
  ReorderConnectionsArgs,
  RollbackTransactionArgs,
  RunAiReadArgs,
  RunQueryArgs,
  RunSort,
  SaveAiSettingsArgs,
  SchemaSnapshot,
  SchemaSnapshotArgs,
  SetAiKeyArgs,
  SqlExportRequest,
  TabStatus,
  TabStatusArgs,
  TableDdlArgs,
  TableExportRequest,
  TableInfo,
  TableProperties,
  TablePropertiesArgs,
  TestAiArgs,
  TestConnectionArgs,
  TestResult,
  TypedValue,
  UpdateConnectionArgs,
  WriteInfo,
} from './generated/wire';

/** A JSON value as the backend sends it: numbers, strings (bigint/numeric too), bool, null, JSON values, bytes. */
export type Cell = CellValue;
export type Row = Record<string, Cell>;
export type Schema = Record<string, string[]>;

/** Binary cell value; `$bytes` is base64 of the preview (`PREVIEW_BYTES`), `len` the full size. */
export interface BytesValue {
  $bytes: string;
  len: number;
}

export type DocumentExportFormat = DocumentFormat | 'csv';

export interface QueryResult {
  columns: string[];
  rows: Row[];
  ms: number;
  /** A statement that changed rows or returned none, as one line of text. */
  note?: string;
  error?: string;
  /** Tab label when the result is not a table (e.g. "Messages"). */
  title?: string;
  sql?: string;
  /** Edit metadata of a result the backend can edit, loaded on the first edit attempt. */
  edit?: EditableResult;
  editReason?: string;
  types?: string[];
  truncated?: boolean;
  limit?: number;
  database?: string;
  summary?: string[];
  /** The shown statement is a plain read, so "Load more" and export may run it again. */
  isRead?: boolean;
  /** Server-side sort of this result, kept when the user switches result tabs. */
  sort?: RunSort;
  /** The shown statement is a query the backend can add ORDER BY to (not EXPLAIN / SHOW). */
  sortable?: boolean;
  notices?: string[];
  /** MongoDB only: each document of the page as relaxed Extended JSON, one per row; the JSON view and Copy value read them. */
  documents?: unknown[];
  /** MongoDB only: BSON type of each cell, same shape as `rows`; "missing" when the document has no such field. */
  cellTypes?: string[][];
  /** MongoDB edits: full cell values loaded with `cellValue`, by row index and path; an update uses them as its old values. */
  loaded?: Record<number, Record<string, TypedValue>>;
}

export class ApiError extends Error {
  code: ErrorCode;
  detail?: string;

  constructor(body: AppError) {
    super(body.message);
    // `Error` subclasses break as invoke errors; a subclass must name its own type.
    this.name = 'ApiError';
    this.code = body.code;
    this.detail = body.detail;
  }

  static from(e: unknown): ApiError {
    if (e instanceof ApiError) return e;
    if (e && typeof e === 'object' && 'code' in e && 'message' in e) return new ApiError(e as AppError);
    return new ApiError({ code: 'internal', message: e instanceof Error ? e.message : String(e) });
  }
}

export function errorText(e: unknown): string {
  const err = ApiError.from(e);
  return err.detail ? `${err.message}: ${err.detail}` : err.message;
}

/** What a transport can do. The UI reads these instead of guessing the transport. */
export interface Capabilities {
  /** Native save dialogs write exported files. */
  fileExport: boolean;
  quitPrompt: boolean;
  unsavedFlag: boolean;
  nativeMenu: boolean;
  windowEffects: boolean;
  pageUnload: boolean;
  updates: boolean;
  secretStorage: 'encrypted' | 'memory' | 'none';
  /** An AI provider is reachable: test and model list. The chat works on every transport. */
  ai: boolean;
}

/** The commands every transport implements. The argument types are the Rust command arguments. */
export interface Backend {
  capabilities: Capabilities;
  listConnections(): Promise<Connection[]>;
  createConnection(args: CreateConnectionArgs): Promise<Connection>;
  updateConnection(args: UpdateConnectionArgs): Promise<Connection>;
  duplicateConnection(args: DuplicateConnectionArgs): Promise<Connection>;
  deleteConnection(args: DeleteConnectionArgs): Promise<void>;
  reorderConnections(args: ReorderConnectionsArgs): Promise<Connection[]>;
  testConnection(args: TestConnectionArgs): Promise<TestResult>;
  listDatabases(args: ListDatabasesArgs): Promise<string[]>;
  listSchemas(args: ListSchemasArgs): Promise<string[]>;
  listTables(args: ListTablesArgs): Promise<TableInfo[]>;
  listColumns(args: ListColumnsArgs): Promise<ColumnInfo[]>;
  tableProperties(args: TablePropertiesArgs): Promise<TableProperties>;
  schemaSnapshot(args: SchemaSnapshotArgs): Promise<SchemaSnapshot>;
  runQuery(args: RunQueryArgs): Promise<QueryRun>;
  editableResult(args: EditableResultArgs): Promise<EditableResult>;
  applyEdits(args: ApplyEditsArgs): Promise<ApplyEditsResult>;
  cellValue(args: CellValueArgs): Promise<string | null>;
  cancelQuery(args: CancelQueryArgs): Promise<void>;
  queryProgress(args: QueryProgressArgs): Promise<QueryProgress | null>;
  commitTransaction(args: CommitTransactionArgs): Promise<void>;
  rollbackTransaction(args: RollbackTransactionArgs): Promise<void>;
  tabStatus(args: TabStatusArgs): Promise<TabStatus | null>;
  closeTabSession(args: CloseTabSessionArgs): Promise<void>;
  quitApp(): Promise<void>;
  /** A no-op without `unsavedFlag`. */
  setUnsavedEdits(value: boolean): Promise<void>;
  /** Without `fileExport` the UI exports in the browser. */
  exportCsv(request: ExportRequest): Promise<ExportResult>;
  exportResultSql(request: SqlExportRequest): Promise<ExportResult>;
  exportDocuments(request: DocumentExportRequest): Promise<ExportResult>;
  tableDdl(args: TableDdlArgs): Promise<string>;
  exportTableSql(request: TableExportRequest): Promise<ExportResult>;
  /** Without `fileExport` the UI downloads in the browser. */
  savePdf(name: string, pdf: Uint8Array): Promise<string | null>;
  parseMongoUri(args: ParseMongoUriArgs): Promise<MongoUriParts | null>;
  guardCheck(args: GuardCheckArgs): Promise<GuardCheck>;
  classifyWrite(args: ClassifyWriteArgs): Promise<WriteInfo>;
  runAiRead(args: RunAiReadArgs): Promise<AiReadResult>;
  getAiSettings(): Promise<AiSettingsView>;
  saveAiSettings(args: SaveAiSettingsArgs): Promise<AiSettingsView>;
  setAiKey(args: SetAiKeyArgs): Promise<void>;
  testAi(args: TestAiArgs): Promise<AiTestResult>;
  listAiModels(args: ListAiModelsArgs): Promise<string[]>;
  /** Streams one reply to `onEvent` and resolves after the terminal `done` or `error` event. */
  chatSend(request: ChatRequest, onEvent: (e: ChatEvent) => void): Promise<void>;
  chatCancel(args: ChatCancelArgs): Promise<void>;
  approveWrite(args: ApproveWriteArgs): Promise<void>;
  denyWrite(args: DenyWriteArgs): Promise<void>;
}
