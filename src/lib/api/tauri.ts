import { invoke, Channel } from '@tauri-apps/api/core';
import { ApiError, type Backend, type ChatEvent, type ChatRequest } from './types';

async function call<T>(cmd: string, args: Record<string, unknown> | Uint8Array = {}, headers?: Record<string, string>): Promise<T> {
  try {
    return await invoke<T>(cmd, args, headers && { headers });
  } catch (e) {
    throw ApiError.from(e);
  }
}

// Channel messages and the command's return travel on different paths, so the last events can arrive after
// the invoke returns. Wait a short time for the terminal event before the promise resolves.
async function chatSend(request: ChatRequest, onEvent: (e: ChatEvent) => void): Promise<void> {
  let ended: () => void = () => {};
  const terminal = new Promise<void>((r) => (ended = r));
  const channel = new Channel<ChatEvent>();
  channel.onmessage = (e) => {
    onEvent(e);
    if (e.type === 'done' || e.type === 'error') ended();
  };
  await call('chat_send', { request, onEvent: channel });
  await Promise.race([terminal, new Promise((r) => setTimeout(r, 2000))]);
}

export const tauriBackend: Backend = {
  kind: 'tauri',
  listConnections: () => call('list_connections'),
  createConnection: (input, password) => call('create_connection', { input, password }),
  updateConnection: (id, input, password) => call('update_connection', { id, input, password }),
  duplicateConnection: (id, input, password) => call('duplicate_connection', { id, input, password }),
  deleteConnection: (id) => call('delete_connection', { id }),
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
  closeTabSession: (tabId) => call('close_tab_session', { tabId }),
  quitApp: () => call('quit_app'),
  exportCsv: (connId, database, sql, tabId) => call('export_csv', { request: { connId, database, sql, tabId } }),
  tableDdl: (connId, database, schema, table) => call('table_ddl', { connId, database, schema, table }),
  exportTableSql: (connId, database, schema, table, data) => call('export_table_sql', { request: { connId, database, schema, table, data } }),
  savePdf: (name, pdf) => call('save_pdf', pdf, { 'x-file-name': name }),
  guardCheck: (engine, sql) => call('guard_check', { engine, sql }),
  classifyWrite: (engine, sql) => call('classify_write', { engine, sql }),
  runAiRead: (connId, database, sql) => call('run_ai_read', { connId, database, sql }),
  chatgptStatus: () => call('chatgpt_status'),
  chatgptSignIn: (enableUsage) => call('chatgpt_sign_in', { enableUsage }),
  chatgptCancelSignIn: () => call('chatgpt_cancel_sign_in'),
  chatgptLogout: () => call('chatgpt_logout'),
  chatgptModels: () => call('chatgpt_models'),
  chatgptManageUsage: () => call('chatgpt_manage_usage'),
  getAiSettings: () => call('get_ai_settings'),
  saveAiSettings: (settings) => call('save_ai_settings', { settings }),
  setAiKey: (key) => call('set_ai_key', { key }),
  testAi: (settings, key) => call('test_ai', { settings, key }),
  listAiModels: (settings, key) => call('list_ai_models', { settings, key }),
  chatSend,
  chatCancel: (chatId) => call('chat_cancel', { chatId }),
};
