import { api, ApiError, type DocumentExportFormat, type EjsonFlavor, type ExportResult, type QueryResult, type SqlOptions } from './api';
import { ask } from './ask.svelte';
import type { Conn } from './connections.svelte';
import { prepareEdit } from './edits';
import { csvCell } from './results';
import { relationsIn } from './sql/columns';
import ExportDocumentsDialog from './components/ExportDocumentsDialog.svelte';
import ExportSqlDialog from './components/ExportSqlDialog.svelte';

/** What the grid's export line shows; null when nothing was written (the user cancelled). */
export interface ExportMessage {
  ok: boolean;
  text: string;
}

/** The desktop app streams every row of a single read to a file it asks for; otherwise the loaded rows export in the browser. */
export async function exportCsv(result: QueryResult, conn: Conn, tabId: string): Promise<ExportMessage | null> {
  if (api.capabilities.fileExport && result.sql && result.isRead && !result.summary) {
    try {
      const out = await api.exportCsv({ connId: conn.id, database: result.database ?? conn.database, sql: result.sql, tabId });
      return out.cancelled ? null : { ok: true, text: `Exported ${out.rows} rows to ${out.path}` };
    } catch (e) {
      return { ok: false, text: ApiError.from(e).message };
    }
  }
  const csv = [result.columns.map(csvCell).join(','), ...result.rows.map((x) => result.columns.map((c) => csvCell(x[c])).join(','))].join('\r\n');
  const a = Object.assign(document.createElement('a'), { href: URL.createObjectURL(new Blob([csv], { type: 'text/csv' })), download: 'result.csv' });
  a.click();
  URL.revokeObjectURL(a.href);
  return null;
}

/**
 * The document export needs a live read the backend can run again; otherwise the loaded rows export
 * in the browser. The dialog asks for the format.
 */
export async function exportDocuments(result: QueryResult, conn: Conn, tabId: string): Promise<ExportMessage | null> {
  if (!(api.capabilities.fileExport && result.sql && result.isRead && !result.summary)) return exportCsv(result, conn, tabId);
  const sql = result.sql;
  const database = result.database ?? conn.database;
  const answer = await ask<{ format: DocumentExportFormat; out: ExportResult } | null>(ExportDocumentsDialog, {
    run: (format: DocumentExportFormat, ejson: EjsonFlavor) => (format === 'csv' ? api.exportCsv({ connId: conn.id, database, sql, tabId }) : api.exportDocuments({ connId: conn.id, database, sql, format, ejson })),
  });
  return answer ? { ok: true, text: `Exported ${answer.out.rows} ${answer.format === 'csv' ? 'rows' : 'documents'} to ${answer.out.path}` } : null;
}

/**
 * INSERT statements for every row of the read, to copy rows to another environment. The dialog starts
 * from the table the query reads (the edit target when known) and its primary key as the upsert key.
 * `current` answers the grid's shown result, so a result that changed while the metadata loaded stops.
 */
export async function exportSql(result: QueryResult, conn: Conn, tabId: string, current: () => QueryResult | null): Promise<ExportMessage | null> {
  if (!api.capabilities.fileExport) return { ok: false, text: 'SQL export to a file needs the desktop app' };
  if (!result.sql || !result.isRead || result.summary) return { ok: false, text: 'SQL export needs the result of one SELECT' };
  // A result that cannot be edited has no primary key to offer; the export still works.
  await prepareEdit(result, conn);
  if (current() !== result) return null;
  const rels = result.edit?.table
    ? [[...(result.edit.schema ? [result.edit.schema] : []), result.edit.table]]
    : conn.engine === 'mongo'
      ? []
      : relationsIn(result.sql, conn.engine).map((x) => x.path);
  // `db.schema.table` keeps the last two parts; with no single table the user names it.
  const path = rels.length === 1 ? rels[0].slice(-2) : [''];
  const key = (result.edit?.pk ?? []).map((k) => result.columns.indexOf(k)).filter((i) => i >= 0);
  const sql = result.sql;
  const out = await ask<ExportResult | null>(ExportSqlDialog, {
    columns: result.columns,
    types: result.types,
    engine: conn.engine,
    schema: path.length === 2 ? path[0] : null,
    table: path[path.length - 1],
    key,
    run: (schema: string | null, table: string, options: SqlOptions) => api.exportResultSql({ connId: conn.id, database: result.database ?? conn.database, sql, schema, table, options, tabId }),
  });
  return out ? { ok: true, text: `Exported ${out.rows} rows to ${out.path}` } : null;
}
