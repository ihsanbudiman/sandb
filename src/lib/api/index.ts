import type { Backend } from './types';
import { tauriBackend } from './tauri';
import { mockBackend } from './mock';

// The bridge client is imported only behind the `import.meta.env.DEV` check, so a production build
// drops it (and any bridge token) entirely.
function bridgeBackend(): Backend {
  const load = import('./http').then((m) => m.httpBackend);
  return {
    kind: 'bridge',
    listConnections: () => load.then((b) => b.listConnections()),
    createConnection: (input, password) => load.then((b) => b.createConnection(input, password)),
    updateConnection: (id, input, password) => load.then((b) => b.updateConnection(id, input, password)),
    duplicateConnection: (id, input, password) => load.then((b) => b.duplicateConnection(id, input, password)),
    deleteConnection: (id) => load.then((b) => b.deleteConnection(id)),
    testConnection: (input, password, id) => load.then((b) => b.testConnection(input, password, id)),
    listDatabases: (connId) => load.then((b) => b.listDatabases(connId)),
    listSchemas: (connId, database) => load.then((b) => b.listSchemas(connId, database)),
    listTables: (connId, database, schema) => load.then((b) => b.listTables(connId, database, schema)),
    listColumns: (connId, database, schema, table) => load.then((b) => b.listColumns(connId, database, schema, table)),
    tableProperties: (connId, database, schema, table) => load.then((b) => b.tableProperties(connId, database, schema, table)),
    schemaSnapshot: (connId, database) => load.then((b) => b.schemaSnapshot(connId, database)),
    runQuery: (request) => load.then((b) => b.runQuery(request)),
    editableResult: (request) => load.then((b) => b.editableResult(request)),
    applyEdits: (request) => load.then((b) => b.applyEdits(request)),
    cellValue: (request) => load.then((b) => b.cellValue(request)),
    cancelQuery: (queryId) => load.then((b) => b.cancelQuery(queryId)),
    queryProgress: (queryId) => load.then((b) => b.queryProgress(queryId)),
    commitTransaction: (tabId) => load.then((b) => b.commitTransaction(tabId)),
    rollbackTransaction: (tabId) => load.then((b) => b.rollbackTransaction(tabId)),
    tabStatus: (tabId) => load.then((b) => b.tabStatus(tabId)),
    closeTabSession: (tabId) => load.then((b) => b.closeTabSession(tabId)),
    quitApp: () => load.then((b) => b.quitApp()),
    exportCsv: (connId, database, sql, tabId) => load.then((b) => b.exportCsv(connId, database, sql, tabId)),
    tableDdl: (connId, database, schema, table) => load.then((b) => b.tableDdl(connId, database, schema, table)),
    exportTableSql: (connId, database, schema, table, data) => load.then((b) => b.exportTableSql(connId, database, schema, table, data)),
    savePdf: (name, pdf) => load.then((b) => b.savePdf(name, pdf)),
    guardCheck: (engine, sql) => load.then((b) => b.guardCheck(engine, sql)),
    classifyWrite: (engine, sql) => load.then((b) => b.classifyWrite(engine, sql)),
    runAiRead: (connId, database, sql) => load.then((b) => b.runAiRead(connId, database, sql)),
    chatgptStatus: () => load.then((b) => b.chatgptStatus()),
    chatgptSignIn: (enableUsage) => load.then((b) => b.chatgptSignIn(enableUsage)),
    chatgptCancelSignIn: () => load.then((b) => b.chatgptCancelSignIn()),
    chatgptLogout: () => load.then((b) => b.chatgptLogout()),
    chatgptModels: () => load.then((b) => b.chatgptModels()),
    chatgptManageUsage: () => load.then((b) => b.chatgptManageUsage()),
    getAiSettings: () => load.then((b) => b.getAiSettings()),
    saveAiSettings: (settings) => load.then((b) => b.saveAiSettings(settings)),
    setAiKey: (key) => load.then((b) => b.setAiKey(key)),
    testAi: (settings, key) => load.then((b) => b.testAi(settings, key)),
    listAiModels: (settings, key) => load.then((b) => b.listAiModels(settings, key)),
    chatSend: (request, onEvent) => load.then((b) => b.chatSend(request, onEvent)),
    chatCancel: (chatId) => load.then((b) => b.chatCancel(chatId)),
  };
}

// Tauri app → IPC; dev server with `VITE_BACKEND=bridge` → dev HTTP bridge; otherwise the in-memory mock.
function pick(): Backend {
  if (typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window) return tauriBackend;
  if (import.meta.env.DEV && import.meta.env.VITE_BACKEND === 'bridge') return bridgeBackend();
  return mockBackend;
}

export const api: Backend = pick();

export * from './types';
