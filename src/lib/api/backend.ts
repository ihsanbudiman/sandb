import type { Backend, Capabilities } from './types';
import type { ChatEvent, CommandName } from './generated/wire';

export type CallArgs = Record<string, unknown> | Uint8Array;

export interface Transport {
  call<T>(name: CommandName, args?: CallArgs, headers?: Record<string, string>): Promise<T>;
  stream(name: CommandName, args: Record<string, unknown>, onEvent: (e: ChatEvent) => void): Promise<void>;
}

/**
 * Every `Backend` method, defined once over the transport. Command names and argument shapes are
 * the Rust command table's (`src-tauri/src/commands.rs`); the argument types come from the
 * generated wire contract.
 */
export function makeBackend(transport: Transport, capabilities: Capabilities): Backend {
  const call = <T>(name: CommandName, args?: CallArgs, headers?: Record<string, string>) => transport.call<T>(name, args, headers);
  return {
    capabilities,
    listConnections: () => call('list_connections'),
    createConnection: (args) => call('create_connection', args),
    updateConnection: (args) => call('update_connection', args),
    duplicateConnection: (args) => call('duplicate_connection', args),
    deleteConnection: (args) => call('delete_connection', args),
    reorderConnections: (args) => call('reorder_connections', args),
    testConnection: (args) => call('test_connection', args),
    listDatabases: (args) => call('list_databases', args),
    listSchemas: (args) => call('list_schemas', args),
    listTables: (args) => call('list_tables', args),
    listColumns: (args) => call('list_columns', args),
    tableProperties: (args) => call('table_properties', args),
    schemaSnapshot: (args) => call('schema_snapshot', args),
    runQuery: (args) => call('run_query', args),
    editableResult: (args) => call('editable_result', args),
    applyEdits: (args) => call('apply_edits', args),
    cellValue: (args) => call('cell_value', args),
    cancelQuery: (args) => call('cancel_query', args),
    queryProgress: (args) => call('query_progress', args),
    commitTransaction: (args) => call('commit_transaction', args),
    rollbackTransaction: (args) => call('rollback_transaction', args),
    tabStatus: (args) => call('tab_status', args),
    closeTabSession: (args) => call('close_tab_session', args),
    quitApp: () => call('quit_app'),
    setUnsavedEdits: capabilities.unsavedFlag ? (value) => call('set_unsaved_edits', { value }) : async () => {},
    exportCsv: (request) => call('export_csv', { request }),
    exportResultSql: (request) => call('export_result_sql', { request }),
    exportDocuments: (request) => call('export_documents', { request }),
    tableDdl: (args) => call('table_ddl', args),
    exportTableSql: (request) => call('export_table_sql', { request }),
    savePdf: (name, pdf) => call('save_pdf', pdf, { 'x-file-name': name }),
    parseMongoUri: (args) => call('parse_mongo_uri', args),
    guardCheck: (args) => call('guard_check', args),
    classifyWrite: (args) => call('classify_write', args),
    runAiRead: (args) => call('run_ai_read', args),
    getAiSettings: () => call('get_ai_settings'),
    saveAiSettings: (args) => call('save_ai_settings', args),
    setAiKey: (args) => call('set_ai_key', args),
    testAi: (args) => call('test_ai', args),
    listAiModels: (args) => call('list_ai_models', args),
    chatSend: (request, onEvent) => transport.stream('chat_send', { request }, onEvent),
    chatCancel: (args) => call('chat_cancel', args),
    approveWrite: (args) => call('approve_write', args),
    denyWrite: (args) => call('deny_write', args),
  };
}
