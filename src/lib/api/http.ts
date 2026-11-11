import { ApiError, type Backend, type ChatEvent, type ChatRequest } from './types';

// Dev-only transport to the Rust bridge (`pnpm bridge`). The bridge only allows the origin http://localhost:5199.
const base = ((import.meta.env.VITE_BRIDGE_URL as string | undefined) || 'http://127.0.0.1:58123').replace(/\/+$/, '');
const token = (import.meta.env.VITE_BRIDGE_TOKEN as string | undefined) ?? '';

async function call<T>(cmd: string, args: Record<string, unknown> = {}, keepalive = false): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`${base}/api/${cmd}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
      body: JSON.stringify(args),
      keepalive,
    });
  } catch (e) {
    throw new ApiError({ code: 'io', message: 'Cannot reach the dev bridge', detail: `${base}: ${e instanceof Error ? e.message : e}` });
  }
  const body: unknown = await res.json().catch(() => null);
  if (!res.ok) throw ApiError.from(body ?? { code: 'internal', message: `Bridge returned HTTP ${res.status}` });
  return body as T;
}

// The bridge answers with `text/event-stream`: one `data: <ChatEvent JSON>` line per event, events split by a blank line.
async function chatSend(request: ChatRequest, onEvent: (e: ChatEvent) => void): Promise<void> {
  let res: Response;
  try {
    res = await fetch(`${base}/api/chat_send`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}`, Accept: 'text/event-stream' },
      body: JSON.stringify({ request }),
    });
  } catch (e) {
    throw new ApiError({ code: 'io', message: 'Cannot reach the dev bridge', detail: `${base}: ${e instanceof Error ? e.message : e}` });
  }
  if (!res.ok || !res.body) {
    const body: unknown = await res.json().catch(() => null);
    throw ApiError.from(body ?? { code: 'internal', message: `Bridge returned HTTP ${res.status}` });
  }
  const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
  let buf = '';
  let ended = false;
  const line = (l: string) => {
    if (!l.startsWith('data:')) return;
    let e: ChatEvent;
    try {
      e = JSON.parse(l.slice(5).trim()) as ChatEvent;
    } catch {
      throw new ApiError({ code: 'ai_protocol', message: 'The dev bridge sent an event that is not valid JSON' });
    }
    onEvent(e);
    if (e.type === 'done' || e.type === 'error') ended = true;
  };
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += value;
      const lines = buf.split(/\r?\n/);
      buf = lines.pop() ?? '';
      for (const l of lines) line(l);
    }
    line(buf);
  } catch (e) {
    reader.cancel().catch(() => {});
    if (e instanceof ApiError) throw e;
    throw new ApiError({ code: 'io', message: 'The connection to the dev bridge stopped', detail: e instanceof Error ? e.message : String(e) });
  } finally {
    reader.releaseLock();
  }
  if (!ended) throw new ApiError({ code: 'ai_protocol', message: 'The reply stopped before it was complete' });
}

export const httpBackend: Backend = {
  kind: 'bridge',
  listConnections: () => call('list_connections'),
  createConnection: (input, password) => call('create_connection', { input, password }),
  updateConnection: (id, input, password) => call('update_connection', { id, input, password }),
  duplicateConnection: (id, input, password) => call('duplicate_connection', { id, input, password }),
  deleteConnection: (id) => call('delete_connection', { id }),
  reorderConnections: (ids) => call('reorder_connections', { ids }),
  testConnection: (input, password, id) => call('test_connection', { input, password, id }),
  listDatabases: (connId) => call('list_databases', { connId }),
  listSchemas: (connId, database) => call('list_schemas', { connId, database }),
  listTables: (connId, database, schema) => call('list_tables', { connId, database, schema }),
  listColumns: (connId, database, schema, table) => call('list_columns', { connId, database, schema, table }),
  tableProperties: (connId, database, schema, table) => call('table_properties', { connId, database, schema, table }),
  schemaSnapshot: (connId, database) => call('schema_snapshot', { connId, database }),
  runQuery: (request) => call('run_query', { request }),
  editableResult: (request) => call('editable_result', { request }),
  applyEdits: (request) => call('apply_edits', { request }),
  cellValue: (request) => call('cell_value', { request }),
  cancelQuery: (queryId) => call('cancel_query', { queryId }),
  queryProgress: (queryId) => call('query_progress', { queryId }),
  commitTransaction: (tabId) => call('commit_transaction', { tabId }),
  rollbackTransaction: (tabId) => call('rollback_transaction', { tabId }),
  tabStatus: (tabId) => call('tab_status', { tabId }),
  // keepalive: the page-hide cleanup sends this while the page unloads, which cancels plain requests.
  closeTabSession: (tabId) => call('close_tab_session', { tabId }, true),
  quitApp: () => call('quit_app'),
  // The bridge never writes files on request; the UI exports the loaded rows in the browser instead.
  guardCheck: (engine, sql) => call('guard_check', { engine, sql }),
  classifyWrite: (engine, sql, database) => call('classify_write', { engine, sql, database: database ?? null }),
  runAiRead: (connId, database, sql) => call('run_ai_read', { connId, database, sql }),
  getAiSettings: () => call('get_ai_settings'),
  saveAiSettings: (settings) => call('save_ai_settings', { settings }),
  setAiKey: (key) => call('set_ai_key', { key }),
  testAi: (settings, key) => call('test_ai', { settings, key }),
  listAiModels: (settings, key) => call('list_ai_models', { settings, key }),
  chatSend,
  chatCancel: (chatId) => call('chat_cancel', { chatId }),
  approveWrite: (chatId, id) => call('approve_write', { chatId, id }),
  denyWrite: (chatId, id) => call('deny_write', { chatId, id }),
  exportCsv: () => Promise.reject(new ApiError({ code: 'invalid_input', message: 'CSV export to a file needs the desktop app' })),
  exportResultSql: () => Promise.reject(new ApiError({ code: 'invalid_input', message: 'SQL export to a file needs the desktop app' })),
  exportDocuments: () => Promise.reject(new ApiError({ code: 'invalid_input', message: 'Document export to a file needs the desktop app' })),
  tableDdl: (connId, database, schema, table) => call('table_ddl', { connId, database, schema, table }),
  exportTableSql: () => Promise.reject(new ApiError({ code: 'invalid_input', message: 'SQL export to a file needs the desktop app' })),
  savePdf: () => Promise.reject(new ApiError({ code: 'invalid_input', message: 'Saving a PDF to a file needs the desktop app' })),
};
