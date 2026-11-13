import type { DocumentExportFormat, EjsonFlavor, InsertMode } from './api';

/** Choices the export and schema dialogs keep for their next opening, for this session only. */
export const exportSqlPrefs = $state({ mode: 'insert' as InsertMode, rowsPerStatement: 100, transaction: false, header: true });

export const exportDocumentsPrefs = $state({ format: 'json' as DocumentExportFormat, ejson: 'relaxed' as EjsonFlavor });

export const validatorPrefs = $state({ level: 'strict', action: 'error' });
