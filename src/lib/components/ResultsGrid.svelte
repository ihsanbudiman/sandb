<script lang="ts">
  import { untrack } from 'svelte';
  import { Download, FileCode, ArrowUpDown, ArrowUp, ArrowDown, ArrowUpRight, Check, X, Pencil, Plus, Minus, Eye, Undo2, CircleSlash, LoaderCircle, CircleAlert, CircleCheck, Copy, Braces } from '@lucide/svelte';
  import { pkOf, tableData, type QueryResult, type Cell, type Row, type Schema } from '../mock';
  import { canWrite, type Conn } from '../state.svelte';
  import { activeTab, tabs } from '../tabs.svelte';
  import { edits, editBlocker, prepareEdit, pendingCount, resetEdits, setCell, addRow, parseInput, buildStatements, saveEdits, MONGO_SCALARS, mongoOriginal, mongoId, mongoLiteral, loadMongoCell } from '../edits.svelte';
  import EditSaveDialog from './EditSaveDialog.svelte';
  import MongoValueDialog from './MongoValueDialog.svelte';
  import ExportSqlDialog from './ExportSqlDialog.svelte';
  import ExportDocumentsDialog from './ExportDocumentsDialog.svelte';
  import JsonTree from './JsonTree.svelte';
  import { api, ApiError, type ForeignKeyInfo, type ResultSort, type SqlExportOptions, type DocumentFormat, type EjsonMode, type TypedValue } from '../api';
  import { copyValue, csvCell, formatCell, isBytes, isCutOff, moreRows } from '../results';
  import { relationsIn } from '../sql/columns';

  /** `foreignKeys` + `onfollow`: a table Data view; cells of a foreign key column get a link to the referenced row. */
  let {
    result,
    conn,
    schema,
    loading = false,
    onloadmore,
    onsaved,
    sort = null,
    onsort,
    foreignKeys = [],
    onfollow,
  }: { result: QueryResult | null; conn: Conn; schema: Schema; loading?: boolean; onloadmore?: () => void; onsaved: () => void; sort?: ResultSort | null; onsort?: (sort: ResultSort) => void; foreignKeys?: ForeignKeyInfo[]; onfollow?: (fk: ForeignKeyInfo, row: Row) => void } = $props();

  // Column → the first foreign key it belongs to; only keys whose columns are all in the result.
  const fkOf = $derived.by(() => {
    const m = new Map<string, ForeignKeyInfo>();
    if (!onfollow || !result) return m;
    for (const fk of foreignKeys) {
      if (!fk.columns.every((c) => result.columns.includes(c))) continue;
      for (const c of fk.columns) if (!m.has(c)) m.set(c, fk);
    }
    return m;
  });
  // A link needs every key value as saved: no NULL, no binary or JSON value, no unsaved change.
  const linkOf = (r: GridRow, c: string) => {
    const fk = fkOf.get(c);
    if (!fk || r.isNew || isDeleted(r)) return null;
    return fk.columns.every((k) => r.row[k] !== null && typeof r.row[k] !== 'object' && !isChanged(r, k)) ? fk : null;
  };

  /** key: `r<index>` for result rows, `a<n>` for rows added in edit mode. */
  type GridRow = { key: string; idx: number; row: Row; isNew: boolean };

  const sorted = $derived<GridRow[]>(result?.rows.map((row, idx) => ({ key: 'r' + idx, idx, row, isNew: false })) ?? []);
  const rows = $derived<GridRow[]>([...sorted, ...edits.added.map((a) => ({ key: 'a' + a.key, idx: -1, row: a.row, isNew: true }))]);

  // Mongo results carry their documents: the JSON view, the cell type tips and Copy value key on them.
  const mongoConn = $derived(conn.engine === 'mongo');
  // The desktop app writes the chosen document format to a file; the browser downloads the loaded rows as CSV.
  const fileExport = $derived(mongoConn && api.kind === 'tauri');
  const mongoDocs = $derived(Array.isArray(result?.documents));
  // Mongo edits write to the server, so the in-memory demo results stay read-only.
  const blocker = $derived(canWrite(conn) ? (mongoConn && result && !result.live ? 'MongoDB edits need a live connection' : editBlocker(result)) : 'Read-only connection: edits are off');
  const count = $derived(pendingCount());

  // Columns or JSON view of a Mongo result; the choice stays for this grid instance.
  let view = $state<'columns' | 'json'>('columns');

  // Scrolling near the end of the grid loads the next page. Not while edits are pending: a new result drops them.
  let scroller = $state<HTMLDivElement>();
  const canLoadMore = $derived(!!result && moreRows(result) && !!onloadmore && !loading && !count);
  function loadIfAtEnd() {
    const el = scroller;
    if (el && canLoadMore && el.scrollHeight - el.scrollTop - el.clientHeight < 200) onloadmore?.();
  }
  // Also after each page lands: a pane taller than the rows has no scroll event to wait for.
  $effect(() => {
    if (canLoadMore) untrack(loadIfAtEnd);
  });

  // A new result (or a connection turned read-only) drops edit mode unless the result is still editable.
  $effect(() => {
    if (blocker) edits.on = false;
  });

  // A notice not tied to a row (for example "not editable") goes away by itself; row errors stay until dismissed.
  $effect(() => {
    const err = edits.error;
    if (!err || err.key) return;
    const t = setTimeout(() => {
      if (edits.error === err) edits.error = null;
    }, 4000);
    return () => clearTimeout(t);
  });

  function sortBy(c: string) {
    if (!result?.sortable || result.summary || loading) return;
    onsort?.({ column: c, columnIndex: result.columns.indexOf(c), ascending: sort?.column === c ? !sort.ascending : true });
  }

  const MIN_COLUMN_WIDTH = 48;
  const MAX_COLUMN_WIDTH = 640;
  let columnWidths = $state<number[]>([]);
  let tableWidth = $state<number | null>(null);
  const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

  function measuredColumns(handle: HTMLElement) {
    const table = handle.closest('table');
    const header = table?.tHead?.rows[0];
    if (!table || !header || !result) return null;
    if (columnWidths.length === result.columns.length && tableWidth !== null) return { widths: [...columnWidths], width: tableWidth };
    const widths = Array.from(header.cells).slice(1).map((cell) => cell.getBoundingClientRect().width);
    const width = table.getBoundingClientRect().width;
    columnWidths = [...widths];
    tableWidth = width;
    return { widths, width };
  }

  function setColumnWidth(ci: number, value: number, initial: { widths: number[]; width: number }) {
    const width = Math.round(clamp(value, MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH));
    columnWidths[ci] = width;
    tableWidth = initial.width + width - initial.widths[ci];
  }

  // Pointer capture keeps the moves coming after the pointer leaves the thin handle.
  function resizeColumn(e: PointerEvent, ci: number) {
    if (e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    const initial = measuredColumns(el);
    if (!initial) return;
    const x = e.clientX;
    el.setPointerCapture(e.pointerId);
    el.onpointermove = (m) => setColumnWidth(ci, initial.widths[ci] + m.clientX - x, initial);
    el.onlostpointercapture = () => (el.onpointermove = el.onlostpointercapture = null);
  }

  function keyColumnResize(e: KeyboardEvent, ci: number) {
    const d = { ArrowLeft: -16, ArrowRight: 16 }[e.key];
    if (!d) return;
    e.preventDefault();
    const initial = measuredColumns(e.currentTarget as HTMLElement);
    if (initial) setColumnWidth(ci, initial.widths[ci] + d, initial);
  }

  let exportMsg = $state<{ ok: boolean; text: string } | null>(null);
  $effect(() => {
    result;
    exportMsg = null;
    sqlExport = null;
  });

  // The desktop app streams every row of a single read to a file it asks for; otherwise the loaded rows export in the browser.
  async function exportCsv() {
    if (!result) return;
    const r = result;
    if (api.kind === 'tauri' && r.live && r.sql && r.isRead && !r.summary) {
      try {
        // The grid shows the active tab's result; its tab id reads inside the tab's open transaction.
        const out = await api.exportCsv(conn.id, r.database ?? conn.database, r.sql, activeTab()?.id);
        if (!out.cancelled) exportMsg = { ok: true, text: `Exported ${out.rows} rows to ${out.path}` };
      } catch (e) {
        exportMsg = { ok: false, text: ApiError.from(e).message };
      }
      return;
    }
    const csv = [r.columns.map(csvCell).join(','), ...sorted.map((x) => r.columns.map((c) => csvCell(x.row[c])).join(','))].join('\r\n');
    const a = Object.assign(document.createElement('a'), { href: URL.createObjectURL(new Blob([csv], { type: 'text/csv' })), download: 'result.csv' });
    a.click();
    URL.revokeObjectURL(a.href);
  }

  // The document export dialog needs a live read the backend can run again; otherwise the loaded rows export in the browser.
  let docExport = $state(false);
  function openDocExport() {
    if (!result) return;
    const r = result;
    if (api.kind === 'tauri' && r.live && r.sql && r.isRead && !r.summary) docExport = true;
    else exportCsv();
  }

  /** Throws so the dialog stays open with the error; closes on success. A closed save dialog keeps it open. */
  async function runDocExport(format: DocumentFormat, ejson: EjsonMode) {
    const r = result;
    if (!r?.sql) return;
    const database = r.database ?? conn.database;
    const out = format === 'csv' ? await api.exportCsv(conn.id, database, r.sql, activeTab()?.id) : await api.exportDocuments(conn.id, database, r.sql, format, ejson);
    if (out.cancelled) return;
    docExport = false;
    exportMsg = { ok: true, text: `Exported ${out.rows} ${format === 'csv' ? 'rows' : 'documents'} to ${out.path}` };
  }

  // INSERT statements for every row of the read, to copy rows to another environment. The dialog starts from the table the
  // query reads (the edit target when known) and its primary key as the upsert key.
  let sqlExport = $state<{ r: QueryResult; schema: string | null; table: string; key: number[] } | null>(null);

  async function exportSql() {
    if (!result) return;
    const r = result;
    if (api.kind !== 'tauri') {
      exportMsg = { ok: false, text: 'SQL export to a file needs the desktop app' };
      return;
    }
    if (!r.live || !r.sql || !r.isRead || r.summary) {
      exportMsg = { ok: false, text: 'SQL export needs the result of one SELECT' };
      return;
    }
    // A result that cannot be edited has no primary key to offer; the export still works.
    await prepareEdit(r, conn);
    if (result !== r) return;
    const rels = r.edit?.table ? [[...(r.edit.schema ? [r.edit.schema] : []), r.edit.table]] : relationsIn(r.sql, conn.engine).map((x) => x.path);
    // `db.schema.table` keeps the last two parts; with no single table the user names it.
    const path = rels.length === 1 ? rels[0].slice(-2) : [''];
    const key = (r.edit?.pk ?? []).map((k) => r.columns.indexOf(k)).filter((i) => i >= 0);
    sqlExport = { r, schema: path.length === 2 ? path[0] : null, table: path[path.length - 1], key };
  }

  /** Throws so the dialog stays open with the error; closes on success. A closed save dialog keeps it open. */
  async function runSqlExport(r: QueryResult, schema: string | null, table: string, options: SqlExportOptions) {
    const out = await api.exportResultSql(conn.id, r.database ?? conn.database, r.sql!, schema, table, options, activeTab()?.id);
    if (out.cancelled) return;
    sqlExport = null;
    exportMsg = { ok: true, text: `Exported ${out.rows} rows to ${out.path}` };
  }

  // ---- edit mode ----
  let selected = $state<Record<string, true>>({});
  let editing = $state<{ key: string; col: string } | null>(null);
  let draft = $state('');
  let menu = $state<{ x: number; y: number; r: GridRow; col: string } | null>(null);
  let dialog = $state<'preview' | 'save' | null>(null);
  /** The save in flight: its id cancels it, and the dialog shows its busy state. */
  let saving = $state(false);
  let saveId = $state<string | null>(null);
  let inputError = $state<string | null>(null);
  let preparing = $state(false);
  /** Full text of the cell open in the editor when the grid shows it cut off; saving it unchanged is not an edit. */
  let full: { key: string; col: string; text: string | null } | null = null;
  let loadingCell = $state(false);
  /** Mongo: the BSON type picked next to the cell editor. */
  let draftType = $state('string');
  let typeSelect = $state<HTMLSelectElement>();
  let cellInput = $state<HTMLInputElement>();
  /** Mongo: a pointer press on the type list; the input's blur that follows must not commit. */
  let pickingType = false;
  /** Mongo: the mongosh value dialog for documents, arrays, "Edit as JSON…" and new documents. */
  let valueEdit = $state<{ title: string; text: string; document: boolean; onsave: (text: string) => void } | null>(null);
  /** Mongo: a read-only tree of one nested cell (document or array), opened outside edit mode. */
  let viewing = $state<{ title: string; value: unknown } | null>(null);

  // Row keys are result indexes, so they point at other rows once the result changes.
  // Only the result identity triggers a reset: prepareEdit sets `result.edit` on the same result, and that must not clear column widths.
  $effect(() => {
    const r = result;
    untrack(() => {
      selected = {};
      pickedCols = {};
      rowAnchor = colAnchor = -1;
      editing = null;
      valueEdit = null;
      viewing = null;
      menu = null;
      columnWidths = [];
      tableWidth = null;
      // Edit mode belongs to the tab's own result: a re-run or another result clears it through
      // `resetEdits`, so a remount must not drop edits that this tab kept while it was inactive.
    });
  });

  // A pending Mongo value is typed: the grid shows its text, NULL for null.
  const value = (r: GridRow, c: string): Cell => {
    if (!isChanged(r, c)) return r.row[c];
    const v = edits.changed[r.idx][c];
    return mongoConn ? ((v as TypedValue).value ?? null) : v;
  };
  const isChanged = (r: GridRow, c: string) => !r.isNew && !!edits.changed[r.idx] && c in edits.changed[r.idx];
  const isDeleted = (r: GridRow) => !r.isNew && !!edits.deleted[r.idx];
  const sample = (c: string) => result?.rows.find((x) => x[c] !== null)?.[c] ?? null;

  async function enableEdit(): Promise<boolean> {
    if (!result || preparing) return false;
    if (blocker) {
      edits.error = { message: blocker, key: '' };
      return false;
    }
    preparing = true;
    const reason = await prepareEdit(result, conn);
    preparing = false;
    if (reason) {
      edits.error = { message: reason, key: '' };
      return false;
    }
    edits.on = true;
    return true;
  }

  async function toggleEdit() {
    if (edits.on && count && !confirm(`Discard ${count} unsaved change(s)?`)) return;
    if (edits.on) resetEdits();
    else if (!(await enableEdit())) return;
    selected = {};
    editing = null;
  }

  function write(r: GridRow, c: string, v: Cell) {
    if (r.isNew) {
      const a = edits.added.find((x) => 'a' + x.key === r.key);
      if (a) a.row[c] = v;
    } else setCell(r.idx, r.row, c, v);
  }

  async function startEdit(r: GridRow, c: string) {
    if (isDeleted(r)) return;
    // Outside edit mode a nested cell opens for reading; Edit turns on editing.
    if (!edits.on && isNested(r, c)) return viewCell(r, c);
    const current = result;
    if (!edits.on || (current?.live && !current.edit)) {
      if (!(await enableEdit())) return;
    }
    if (result !== current || isDeleted(r)) return;
    if (mongoConn) return startMongoEdit(r, c);
    const v = value(r, c);
    if (isBytes(v)) {
      edits.error = { message: 'Binary preview cells are read-only', key: r.key };
      return;
    }
    let text: string | null = v === null ? '' : typeof v === 'object' ? JSON.stringify(v) : String(v);
    full = null;
    // The grid gets at most 4 KB per cell; load the whole value from the row's primary key.
    if (isCutOff(v) && !r.isNew && !isChanged(r, c)) {
      const pk = current?.live ? current.edit?.pk : undefined;
      if (!current || !pk?.length || !current.sql) {
        edits.error = { message: 'Truncated text cells are read-only', key: r.key };
        return;
      }
      if (loadingCell) return;
      loadingCell = true;
      try {
        text = await api.cellValue({
          connId: conn.id,
          database: current.database ?? conn.database,
          sql: current.sql,
          tabId: activeTab()?.id,
          pk: Object.fromEntries(pk.map((k) => [k, r.row[k]])),
          column: c,
        });
      } catch (e) {
        edits.error = { message: ApiError.from(e).message, key: r.key };
        return;
      } finally {
        loadingCell = false;
      }
      if (result !== current || isDeleted(r)) return;
      full = { key: r.key, col: c, text };
      text ??= '';
    }
    draft = text;
    inputError = null;
    editing = { key: r.key, col: c };
  }

  function commit(r: GridRow, c: string, blur = false) {
    // Esc clears `editing` first, so the blur that follows does nothing.
    if (editing?.key !== r.key || editing.col !== c) return;
    if (mongoConn) return commitMongo(r, c, blur);
    const before = value(r, c);
    // A loaded full value saved as it is stays unchanged; its cut-off preview is the original.
    if (full?.key === r.key && full.col === c && draft === (full.text ?? '')) {
      full = null;
      editing = null;
      return;
    }
    const parsed = parseInput(draft, sample(c), result?.types?.[result.columns.indexOf(c)]);
    if (parsed === undefined) {
      // Enter keeps the editor open with the error; leaving the cell drops the invalid draft.
      if (!blur) return void (inputError = 'Invalid value');
    } else if (!(before === null && draft === '')) write(r, c, parsed); // Leaving a NULL cell empty keeps it NULL instead of turning it into ''.
    full = null;
    editing = null;
  }

  // ---- MongoDB: values keep their BSON type ----

  const INT64 = 2n ** 63n;
  const DECIMAL = /^-?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i;
  /** Client-side check of the editor text per type; the backend is authoritative. */
  const MONGO_VALID: Record<string, (s: string) => boolean> = {
    string: () => true,
    int32: (s) => /^-?\d+$/.test(s) && +s >= -(2 ** 31) && +s < 2 ** 31,
    int64: (s) => /^-?\d+$/.test(s) && BigInt(s) >= -INT64 && BigInt(s) < INT64,
    double: (s) => DECIMAL.test(s) || /^(?:NaN|-?Infinity)$/.test(s),
    decimal128: (s) => DECIMAL.test(s) || /^(?:NaN|-?Infinity)$/.test(s),
    bool: (s) => s === 'true' || s === 'false',
    date: (s) => /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/.test(s) && !Number.isNaN(Date.parse(s)),
    objectId: (s) => /^[0-9a-f]{24}$/i.test(s),
    uuid: (s) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s),
  };

  /** The type a missing (or null) field gets when the user types a value: the column's most frequent writable type. */
  const columnType = (c: string) => result?.types?.[result.columns.indexOf(c)]?.split(' | ').find((t) => t in MONGO_VALID) ?? 'string';

  /** Why the row cannot be edited or deleted: its `_id` has no typed form. */
  function idProblem(r: GridRow): string | null {
    if (r.isNew || !result || mongoId(result, r.idx)) return null;
    const ci = result.columns.indexOf('_id');
    return `This document's _id type (${(ci < 0 ? undefined : result.cellTypes?.[r.idx]?.[ci]) ?? 'missing'}) cannot be edited in the grid`;
  }

  /** The cell as it was loaded, typed. Loads the full value by `_id` when the grid cell does not hold it exactly; null on failure. */
  async function mongoLoad(r: GridRow, c: string): Promise<TypedValue | null> {
    const current = result;
    if (!current?.sql) return null;
    const have = mongoOriginal(current, r.idx, c);
    if (have) return have;
    if (loadingCell) return null;
    loadingCell = true;
    try {
      const v = await loadMongoCell(current, conn, r.idx, c, activeTab()?.id);
      return result === current ? v : null;
    } catch (e) {
      edits.error = { message: ApiError.from(e).message, key: r.key };
      return null;
    } finally {
      loadingCell = false;
    }
  }

  /** Stores a typed value; the value as loaded (also as the same mongosh text) clears the change. */
  function writeMongo(r: GridRow, c: string, v: TypedValue) {
    const orig = result ? mongoOriginal(result, r.idx, c) : undefined;
    const same = !!orig && ((orig.type === v.type && orig.value === v.value) || (v.type === 'shell' && orig.type !== 'missing' && mongoLiteral(orig) === v.value));
    setCell(r.idx, r.row, c, same ? r.row[c] : v);
  }

  /** Scalar cells open the inline editor with a type list; documents and arrays open the value dialog. */
  async function startMongoEdit(r: GridRow, c: string) {
    if (r.isNew) return editDocument(r);
    if (editing?.key === r.key && editing.col === c) return;
    const bad = idProblem(r);
    // A document the grid cannot edit still shows its nested values.
    if (bad && isNested(r, c)) return viewCell(r, c);
    if (bad) return void (edits.error = { message: bad, key: r.key });
    const current = result;
    const pending = isChanged(r, c) ? (edits.changed[r.idx][c] as TypedValue) : undefined;
    const type = pending?.type ?? current?.cellTypes?.[r.idx]?.[current.columns.indexOf(c)] ?? 'missing';
    if (type === 'object' || type === 'array' || type === 'shell') return editJson(r, c);
    if (type !== 'missing' && !MONGO_SCALARS.includes(type)) return void (edits.error = { message: `${type} values are read-only in the cell editor; use Edit as JSON…`, key: r.key });
    const v = pending ?? (await mongoLoad(r, c));
    if (!v || result !== current || isDeleted(r)) return;
    // The one-line input drops line breaks; the dialog keeps them as `\n` in the string literal.
    if (v.type === 'string' && /[\r\n]/.test(v.value ?? '')) return editJson(r, c);
    draft = v.value ?? '';
    draftType = v.type === 'missing' ? columnType(c) : v.type;
    inputError = null;
    pickingType = false;
    editing = { key: r.key, col: c };
  }

  /** `commit` for Mongo cells: the text must fit the picked type. */
  function commitMongo(r: GridRow, c: string, blur: boolean) {
    const text = draftType === 'string' ? draft : draft.trim();
    // Leaving a NULL or missing cell empty keeps it as it is instead of turning it into ''.
    if (draftType !== 'null' && text === '' && value(r, c) === null) editing = null;
    else if (draftType === 'null') writeMongo(r, c, { type: 'null', value: null });
    else if (MONGO_VALID[draftType]?.(text)) writeMongo(r, c, { type: draftType, value: text });
    else if (!blur) return void (inputError = 'Invalid value');
    editing = null;
  }

  /** The value dialog with the cell's mongosh text; Apply stores it as `shell`. */
  async function editJson(r: GridRow, c: string) {
    if (r.isNew) return editDocument(r);
    const bad = idProblem(r);
    if (bad) return void (edits.error = { message: bad, key: r.key });
    const current = result;
    const v = isChanged(r, c) ? (edits.changed[r.idx][c] as TypedValue) : await mongoLoad(r, c);
    if (!v || result !== current || isDeleted(r)) return;
    editing = null;
    valueEdit = { title: `Edit ${c}`, text: v.type === 'missing' ? '' : mongoLiteral(v), document: false, onsave: (text) => writeMongo(r, c, { type: 'shell', value: text }) };
  }

  /** Sets the field to null; the update needs its old value, so a cell the grid cannot hold exactly loads first. */
  async function setMongoNull(r: GridRow, c: string) {
    const bad = idProblem(r);
    if (bad) return void (edits.error = { message: bad, key: r.key });
    if (await mongoLoad(r, c)) writeMongo(r, c, { type: 'null', value: null });
  }

  /** A new document, or the text of one added before: its grid row stays all NULL. */
  function editDocument(r?: GridRow) {
    if (!result) return;
    const columns = result.columns;
    const added = r && edits.added.find((a) => 'a' + a.key === r.key);
    valueEdit = {
      title: added ? 'Edit new document' : `Add document to ${result.edit?.table ?? 'the collection'}`,
      text: added?.doc ?? '{\n  \n}',
      document: true,
      onsave: (text) => {
        if (added) added.doc = text;
        else addRow(Object.fromEntries(columns.map((c) => [c, null])), text);
      },
    };
  }

  function focusInput(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

  // Row-number clicks select rows and header clicks select columns, like DBeaver: plain click picks one,
  // ⌘/Ctrl adds or removes one, Shift extends from the last plain or ⌘ click. ⌘C copies the block.
  let pickedCols = $state<Record<string, true>>({});
  let rowAnchor = -1;
  let colAnchor = -1;

  function pick<T>(cur: Record<string, true>, items: T[], i: number, anchor: number, e: MouseEvent, key: (x: T) => string) {
    if (e.shiftKey && anchor >= 0) {
      const out: Record<string, true> = e.metaKey || e.ctrlKey ? { ...cur } : {};
      for (let k = Math.min(anchor, i); k <= Math.max(anchor, i); k++) out[key(items[k])] = true;
      return out;
    }
    if (!(e.metaKey || e.ctrlKey)) return { [key(items[i])]: true } as Record<string, true>;
    const out = { ...cur };
    if (out[key(items[i])]) delete out[key(items[i])];
    else out[key(items[i])] = true;
    return out;
  }

  function pickRow(i: number, e: MouseEvent) {
    selected = pick(selected, rows, i, rowAnchor, e, (r) => r.key);
    if (!e.shiftKey) rowAnchor = i;
    if (!e.shiftKey && !e.metaKey && !e.ctrlKey) pickedCols = {};
    scroller?.focus({ preventScroll: true });
  }

  function pickColumn(ci: number, e: MouseEvent) {
    if (!result) return;
    pickedCols = pick(pickedCols, result.columns, ci, colAnchor, e, (c) => c);
    if (!e.shiftKey) colAnchor = ci;
    if (!e.shiftKey && !e.metaKey && !e.ctrlKey) selected = {};
    scroller?.focus({ preventScroll: true });
  }

  const hasRows = $derived(Object.keys(selected).length > 0);
  const hasCols = $derived(Object.keys(pickedCols).length > 0);
  /** The copied block: picked rows (all if none) × picked columns (all if none). */
  const inBlock = (r: GridRow, c: string) => (hasRows || hasCols) && (!hasRows || !!selected[r.key]) && (!hasCols || !!pickedCols[c]);

  function copyBlock(e: KeyboardEvent) {
    if (!(e.metaKey || e.ctrlKey) || e.altKey || e.key.toLowerCase() !== 'c' || editing || !result || !(hasRows || hasCols)) return;
    e.preventDefault();
    const cols = result.columns.filter((c) => !hasCols || pickedCols[c]);
    const text = rows
      .filter((r) => !hasRows || selected[r.key])
      .map((r) => cols.map((c) => {
        const v = value(r, c);
        return v == null ? '' : typeof v === 'object' ? (isBytes(v) ? v.$bytes : JSON.stringify(v)) : String(v);
      }).join('\t'))
      .join('\n');
    navigator.clipboard?.writeText(text).catch(() => {});
  }

  function deleteRows(keys: string[]) {
    for (const k of keys) {
      const r = rows.find((x) => x.key === k);
      if (!r) continue;
      const bad = mongoConn ? idProblem(r) : null;
      if (bad) edits.error = { message: bad, key: r.key };
      else if (r.isNew) edits.added = edits.added.filter((a) => 'a' + a.key !== k);
      else if (edits.deleted[r.idx]) delete edits.deleted[r.idx];
      else edits.deleted[r.idx] = true;
    }
    selected = {};
  }

  function newRow() {
    if (!result) return;
    if (mongoConn) return editDocument();
    const row: Row = Object.fromEntries(result.columns.map((c) => [c, null]));
    const pk = pkOf(result.table ?? '');
    if (api.kind === 'mock' && result.table && pk.length === 1) {
      const d = tableData(schema, result.table);
      const ids = [...d.base, ...d.added, ...edits.added.map((a) => a.row)].map((x) => x[pk[0]]).filter((v): v is number => typeof v === 'number');
      if (ids.length) row[pk[0]] = Math.max(...ids) + 1;
    }
    addRow(row);
  }

  function openMenu(e: MouseEvent, r: GridRow, c: string) {
    // Mongo cells always have a menu: Copy value works outside edit mode too.
    if (!edits.on && !mongoDocs) return;
    e.preventDefault();
    if (edits.on && !selected[r.key]) selected = { [r.key]: true };
    menu = { x: Math.min(e.clientX, innerWidth - 170), y: Math.min(e.clientY, innerHeight - (mongoDocs ? 220 : 130)), r, col: c };
  }

  /** Full value of a Mongo cell, read from its document at the column path; undefined for a missing field. */
  function docValue(r: GridRow, c: string): unknown {
    let v: unknown = result?.documents?.[r.idx];
    for (const part of c.split('.')) {
      if (v === null || typeof v !== 'object') return undefined;
      v = (v as Record<string, unknown>)[part];
    }
    return v;
  }

  function copyCell(r: GridRow, c: string) {
    const v = docValue(r, c);
    if (v !== undefined) navigator.clipboard?.writeText(copyValue(v)).catch(() => {});
  }

  /** A saved document or array cell of a Mongo result. */
  function isNested(r: GridRow, c: string) {
    if (!mongoDocs || r.isNew || isChanged(r, c)) return false;
    const t = result?.cellTypes?.[r.idx]?.[result.columns.indexOf(c)];
    return t === 'object' || t === 'array';
  }

  function viewCell(r: GridRow, c: string) {
    const v = docValue(r, c);
    if (v !== undefined) viewing = { title: c, value: v };
  }

  async function doSave() {
    if (!result || saving) return;
    saving = true;
    // Tab close and switch wait for the save: a reset after it would wipe the next tab's edits.
    edits.saving = true;
    saveId = crypto.randomUUID();
    let ok = false;
    try {
      ok = await saveEdits(result, conn, activeTab()?.id, saveId ?? undefined);
    } finally {
      saving = false;
      edits.saving = false;
      saveId = null;
    }
    // A failed save keeps the dialog open, so the error shows where the user looks.
    if (ok) {
      dialog = null;
      onsaved();
    }
  }

  /** The dialog's Cancel: the save ends locally; the server kill is best effort. */
  function cancelSave() {
    if (saveId) api.cancelQuery(saveId).catch(() => {});
  }

  const stmts = $derived(dialog && result && !blocker ? buildStatements(result, conn) : []);

  function rowTint(r: GridRow) {
    if (edits.error?.key === r.key) return 'bg-red-500/15 outline outline-1 -outline-offset-1 outline-red-500/60';
    if (isDeleted(r)) return 'bg-red-500/8 text-faint line-through decoration-red-500/60 dark:bg-red-500/10';
    if (r.isNew) return 'bg-emerald-500/8 dark:bg-emerald-500/10';
    if (selected[r.key] && !hasCols) return 'bg-accent/10';
    return 'hover:bg-hover';
  }

  /** The row's tint for the sticky row-number cell: its opaque background hides the tint on the `tr`. */
  function numberTint(r: GridRow) {
    if (edits.error?.key === r.key) return 'bg-red-500/15';
    if (isDeleted(r)) return 'bg-red-500/8 dark:bg-red-500/10';
    if (r.isNew) return 'bg-emerald-500/8 dark:bg-emerald-500/10';
    if (selected[r.key] && !hasCols) return 'bg-accent/10';
    return 'group-hover/row:bg-hover';
  }
</script>

<svelte:window
  onclick={() => (menu = null)}
  onbeforeunload={(e) => { if (pendingCount() || tabs.list.some((t) => t.pending)) e.preventDefault(); }}
  onkeydown={(e) => { if (e.key === 'Escape') { menu = null; viewing = null; } }}
/>

{#if !result}
  <div class="grid h-full place-items-center">
    <div class="text-center">
      <div class="text-[13px] font-medium text-ink">No results yet</div>
      <div class="mt-1 flex items-center justify-center gap-1.5 text-[12.5px] text-muted">Write a query and press <kbd class="kbd">⌘↵</kbd></div>
    </div>
  </div>
{:else if result.error}
  <div class="p-4">
    <div class="flex items-start gap-2.5 rounded-lg bg-red-500/8 px-3.5 py-3" role="alert">
      <CircleAlert size={15} class="mt-px shrink-0 text-red-600 dark:text-red-400" />
      <div class="min-w-0">
        <span class="text-[12.5px] font-semibold text-red-700 dark:text-red-400">Error</span>
        <pre class="mt-1 font-mono text-[12px] leading-relaxed whitespace-pre-wrap text-red-800 dark:text-red-300">{result.error}</pre>
      </div>
    </div>
  </div>
{:else if result.note}
  <div class="flex h-full flex-col gap-3 overflow-auto p-4">
    <div class="flex items-center gap-2.5">
      <CircleCheck size={15} class="shrink-0 text-emerald-600 dark:text-emerald-400" />
      <span><span class="sr-only">Succeeded: </span>{result.note} <span class="font-mono text-[11.5px] text-faint">· {result.ms} ms</span></span>
    </div>
    {#if result.notices}{@render notices(result.notices)}{/if}
    {#if result.summary}{@render summary(result.summary)}{/if}
  </div>
{:else}
  <div class="flex h-full flex-col">
    {#if result.notices}<div class="border-b border-line px-3 py-1.5">{@render notices(result.notices)}</div>{/if}
    {#if result.summary}<div class="border-b border-line px-3 py-1.5">{@render summary(result.summary)}</div>{/if}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="min-h-0 flex-1 overflow-auto outline-none" tabindex="-1" bind:this={scroller} onscroll={loadIfAtEnd} onkeydown={copyBlock}>
      {#if mongoDocs && view === 'json'}
        <JsonTree documents={result.documents ?? []} more={moreRows(result)} {loading} onloadmore={onloadmore} />
      {:else}
      <table class="w-full border-separate border-spacing-0 font-mono text-[12px] {tableWidth === null ? '' : 'table-fixed'}" style:width={tableWidth === null ? undefined : `${tableWidth}px`}>
        <colgroup>
          <col class="w-9" />
          {#each result.columns as _, ci}
            <col style:width={columnWidths[ci] === undefined ? undefined : `${columnWidths[ci]}px`} />
          {/each}
        </colgroup>
        <thead class="sticky top-0 z-10">
          <tr>
            <th class="sticky left-0 w-9 border-b border-line bg-surface px-2 py-0 text-right font-sans text-[10.5px] font-normal text-faint shadow-[1px_0_0_var(--c-line)]"></th>
            {#each result.columns as c, ci}
              <th class="group/th relative h-8 border-b border-l border-line bg-surface px-2.5 py-0 text-left font-normal" title={result.types?.[ci]}>
                {#if pickedCols[c]}<span class="pointer-events-none absolute inset-0 bg-accent/10"></span>{/if}
                <div class="relative flex w-full items-center gap-1.5">
                  <button class="flex min-w-0 flex-1 items-baseline gap-1.5 text-left whitespace-nowrap select-none" title="Select column (⌘ add, ⇧ range)" onclick={(e) => pickColumn(ci, e)}>
                    <span class="font-sans text-[12px] font-semibold {pickedCols[c] || sort?.column === c ? 'text-accent' : 'text-ink'}">{c}</span>
                    {#if result.types?.[ci]}<span class="max-w-72 min-w-0 truncate text-[10.5px] text-faint">{result.types?.[ci]}</span>{/if}
                  </button>
                  {#if result.sortable}
                    <button
                      class="shrink-0 rounded p-0.5 hover:bg-hover {sort?.column === c ? '' : 'opacity-0 transition-opacity group-hover/th:opacity-100 focus-visible:opacity-100'}"
                      title="Sort by {c}"
                      aria-label="Sort by {c}"
                      onclick={() => sortBy(c)}
                    >
                      {#if sort?.column === c}
                        {#if sort?.ascending}<ArrowUp size={12} class="text-accent" />{:else}<ArrowDown size={12} class="text-accent" />{/if}
                      {:else}<ArrowUpDown size={12} class="text-faint" />{/if}
                    </button>
                  {/if}
                </div>
                <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
                <div
                  class="absolute inset-y-0 -right-1 z-20 w-2 cursor-col-resize hover:bg-accent/25 focus-visible:bg-accent/40"
                  role="separator"
                  aria-orientation="vertical"
                  aria-label={`Resize ${c} column`}
                  aria-valuemin={MIN_COLUMN_WIDTH}
                  aria-valuemax={MAX_COLUMN_WIDTH}
                  aria-valuenow={columnWidths[ci]}
                  title={`Resize ${c} column`}
                  tabindex="0"
                  onfocus={(e) => measuredColumns(e.currentTarget)}
                  onpointerdown={(e) => resizeColumn(e, ci)}
                  onkeydown={(e) => keyColumnResize(e, ci)}
                ></div>
              </th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each rows as r, i (r.key)}
            <tr class="group/row {rowTint(r)}">
              <td
                class="sticky left-0 z-[1] cursor-pointer border-b border-line/60 bg-surface px-2 text-right text-[10.5px] tabular-nums shadow-[1px_0_0_var(--c-line)] select-none {selected[r.key] ? 'font-medium text-ink' : 'text-faint'}"
                onclick={(e) => pickRow(i, e)}
              ><span class="pointer-events-none absolute inset-0 {numberTint(r)}"></span><span class="relative">{r.isNew ? '+' : i + 1}</span></td>
              {#each result.columns as c, ci}
                {@const v = value(r, c)}
                {@const isEd = editing?.key === r.key && editing.col === c}
                {@const fk = linkOf(r, c)}
                <td
                  title={mongoDocs ? (r.isNew ? 'new document' : isChanged(r, c) ? (edits.changed[r.idx][c] as TypedValue).type : (result.cellTypes?.[r.idx]?.[ci] ?? 'missing')) : undefined}
                  class="relative h-[26px] {tableWidth === null ? 'max-w-72' : ''} truncate border-b border-l border-line/60 {fk ? 'pr-7 pl-2.5' : 'px-2.5'} {isChanged(r, c) && !isDeleted(r) ? 'bg-amber-400/15 dark:bg-amber-400/12' : hasCols && inBlock(r, c) ? 'bg-accent/10' : ''} {typeof v === 'number' ? 'text-right' : ''}"
                  ondblclick={() => startEdit(r, c)}
                  onclick={(e) => {
                    if (e.metaKey || e.ctrlKey) return;
                    pickedCols = {};
                    selected = edits.on ? { [r.key]: true } : {};
                  }}
                  oncontextmenu={(e) => openMenu(e, r, c)}
                >
                  {#if isEd}
                    <input
                      class="absolute inset-0 block h-full w-full min-w-0 bg-field font-mono text-[12px] outline-2 -outline-offset-2 {mongoConn ? 'pr-[6.5rem] pl-2.5' : 'px-2.5'} {inputError ? 'outline-red-500' : 'outline-accent'}"
                      title={inputError}
                      aria-invalid={!!inputError}
                      bind:this={cellInput}
                      oninput={() => {
                        inputError = null;
                        // Typing into a null cell picks a type that can hold the text.
                        if (mongoConn && draftType === 'null') draftType = columnType(c);
                      }}
                      bind:value={draft}
                      use:focusInput
                      placeholder="NULL"
                      autocapitalize="off"
                      autocorrect="off"
                      spellcheck="false"
                      autocomplete="off"
                      onkeydown={(e) => {
                        if (e.key === 'Enter') commit(r, c);
                        else if (e.key === 'Escape') { e.stopPropagation(); editing = null; }
                      }}
                      onfocus={() => (pickingType = false)}
                      onblur={(e) => {
                        // Moving to the type list is not leaving the cell.
                        if (mongoConn && (pickingType || e.relatedTarget === typeSelect)) return;
                        commit(r, c, true);
                      }}
                    />
                    {#if mongoConn}
                      <select
                        class="absolute inset-y-0 right-0 w-24 border-l border-line bg-field px-1 font-sans text-[11px] text-muted outline-none"
                        aria-label="BSON type"
                        title="BSON type"
                        bind:this={typeSelect}
                        bind:value={draftType}
                        onpointerdown={() => (pickingType = true)}
                        onchange={() => {
                          inputError = null;
                          cellInput?.focus();
                        }}
                        onkeydown={(e) => {
                          if (e.key === 'Enter') commit(r, c);
                          else if (e.key === 'Escape') { e.stopPropagation(); editing = null; }
                        }}
                        onblur={(e) => {
                          pickingType = false;
                          if (e.relatedTarget !== cellInput) commit(r, c, true);
                        }}
                        ondblclick={(e) => e.stopPropagation()}
                      >
                        {#each MONGO_SCALARS as t (t)}<option value={t}>{t}</option>{/each}
                      </select>
                    {/if}
                  {/if}
                  {#if fk && !isEd}
                    {@const target = (fk.refSchema ? fk.refSchema + '.' : '') + fk.refTable}
                    <button
                      class="absolute top-1/2 right-1.5 inline-flex size-4 -translate-y-1/2 items-center justify-center rounded text-faint opacity-60 group-hover/row:opacity-100 hover:bg-accent/15 hover:text-accent"
                      title="Open {target} where {fk.refColumns.map((rc, k) => `${rc} = ${String(r.row[fk.columns[k]])}`).join(' and ')}"
                      aria-label="Open referenced row in {target}"
                      onclick={(e) => { e.stopPropagation(); onfollow?.(fk, r.row); }}
                      ondblclick={(e) => e.stopPropagation()}
                    ><ArrowUpRight size={12} /></button>
                  {/if}
                  <span class:invisible={isEd}>
                    {#if v === null}
                      <span class="text-[11px] italic {selected[r.key] ? 'text-muted' : 'text-faint'}">NULL</span>
                    {:else if typeof v === 'boolean'}
                      {#if v}<Check size={14} class="text-emerald-600 dark:text-emerald-400" />{:else}<X size={14} class="text-faint" />{/if}
                    {:else if typeof v === 'number'}
                      <span class="tabular-nums">{v}</span>
                    {:else if typeof v === 'object'}
                      <span class="text-muted">{formatCell(v)}</span>
                    {:else}
                      {v}
                    {/if}
                  </span>
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
      {/if}
    </div>
    {#if edits.error}
      <div class="flex items-center gap-2 border-t border-red-500/25 bg-red-500/6 px-3 py-1.5 text-[12px] text-red-700 dark:text-red-400">
        <CircleAlert size={13} class="shrink-0" />
        <span class="sr-only">ERROR</span>
        <span class="min-w-0 flex-1 truncate" title={edits.error.message}>{edits.error.message}</span>
        <button class="btn btn-ghost h-5 px-1 text-[11px]" onclick={() => (edits.error = null)} aria-label="Dismiss"><X size={12} /></button>
      </div>
    {/if}
    <footer class="@container flex h-9 shrink-0 items-center gap-1 border-t border-line px-2 text-[12px] text-muted">
      <span title={blocker ?? (edits.on ? 'Leave edit mode' : mongoConn ? 'Edit documents in this result' : 'Edit rows in this result')}>
        <button
          class="btn h-7 px-2 text-[12px] {edits.on ? 'bg-accent/6 text-accent' : 'btn-ghost'}"
          disabled={!!blocker || preparing}
          title={blocker ?? undefined}
          onclick={toggleEdit}
        ><Pencil size={13} /> Edit</button>
      </span>
      {#if edits.on}
        <span class="px-1 text-[12px] font-medium text-accent">Editing</span>
        <button class="btn btn-ghost h-7 px-2 text-[12px]" title={mongoConn ? 'Add document' : 'Add row'} onclick={newRow}><Plus size={13} /><span class="hidden @[36rem]:inline">{mongoConn ? 'Document' : 'Row'}</span></button>
        <button class="btn btn-ghost h-7 px-2 text-[12px]" title={mongoConn ? 'Delete selected documents' : 'Delete selected rows'} disabled={!Object.keys(selected).length} onclick={() => deleteRows(Object.keys(selected))}><Minus size={13} /><span class="hidden @[36rem]:inline">Delete</span></button>
        <span class="mx-1 h-4 w-px bg-line"></span>
      {/if}
      {#if count}
        <span class="px-1 text-[12px] font-medium text-ink">{count} change{count === 1 ? '' : 's'}</span>
        <button class="btn btn-ghost h-7 px-2 text-[12px]" title="Discard changes" onclick={() => resetEdits(true)}><Undo2 size={13} /><span class="hidden @[36rem]:inline">Discard</span></button>
        <button class="btn btn-ghost h-7 px-2 text-[12px]" title={mongoConn ? 'Preview commands' : 'Preview SQL'} onclick={() => (dialog = 'preview')}><Eye size={13} /><span class="hidden @[36rem]:inline">{mongoConn ? 'Preview commands' : 'Preview SQL'}</span></button>
        <button class="btn btn-primary h-7 px-3 text-[12px]" onclick={() => (dialog = 'save')}>Save</button>
      {:else}
        <span class="flex items-center gap-2.5 px-1.5 text-[12px] text-faint tabular-nums">
          <span title={result.truncated ? `First ${result.rows.length} rows; ${result.isRead ? (moreRows(result) ? 'scroll down to load more' : 'the page reached its size limit, so loading more brings no new rows') : 'more exist. Only a read-only statement loads more, because loading runs the SQL again'}` : undefined}>{rows.length}{result.truncated ? '+' : ''} rows</span>
          <span class="size-0.5 rounded-full bg-faint"></span>
          <span>{result.ms} ms</span>
        </span>
        {#if result.truncated && onloadmore && loading}
          <span class="flex items-center gap-1.5 px-1 text-[12px] text-faint"><LoaderCircle size={13} class="animate-spin" /> Loading more…</span>
        {/if}
      {/if}
      {#if mongoDocs}
        <div class="seg ml-2 shrink-0" role="tablist" aria-label="Result view">
          <button class="seg-item {view === 'columns' ? 'seg-on' : ''}" role="tab" aria-selected={view === 'columns'} onclick={() => (view = 'columns')}>Columns</button>
          <button class="seg-item {view === 'json' ? 'seg-on' : ''}" role="tab" aria-selected={view === 'json'} onclick={() => (view = 'json')}>JSON</button>
        </div>
      {/if}
      {#if exportMsg}
        <span class="min-w-0 truncate text-[12px] {exportMsg.ok ? 'text-emerald-700 dark:text-emerald-400' : 'text-red-600 dark:text-red-400'}" title={exportMsg.text}>{exportMsg.text}</span>
      {/if}
      <!-- A MongoDB result has no INSERT statements; the document formats come from the export dialog. -->
      {#if !mongoConn}
        <button class="btn btn-ghost ml-auto h-7 px-2 text-[12px]" title="Export as SQL INSERT statements" onclick={exportSql}><FileCode size={13} /><span class="hidden @[36rem]:inline">Export SQL</span></button>
      {/if}
      <button class="btn btn-ghost h-7 px-2 text-[12px] {mongoConn ? 'ml-auto' : ''}" title={fileExport ? 'Export the documents as CSV, JSON or NDJSON' : 'Export CSV'} onclick={fileExport ? openDocExport : exportCsv}><Download size={13} /><span class="hidden @[36rem]:inline">{fileExport ? 'Export...' : 'Export CSV'}</span></button>
    </footer>
  </div>
{/if}

{#if menu}
  {@const m = menu}
  <div class="popover fixed z-40 w-40 p-1" style="left:{m.x}px;top:{m.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    {#if mongoDocs}
      {#if edits.on && m.r.isNew}
        <button class="nav-item" onclick={() => { editDocument(m.r); menu = null; }}><Braces size={13} /> Edit document…</button>
      {:else if edits.on && !isDeleted(m.r)}
        <button class="nav-item" onclick={() => { startEdit(m.r, m.col); menu = null; }}><Pencil size={13} /> Edit cell</button>
        <button class="nav-item" onclick={() => { editJson(m.r, m.col); menu = null; }}><Braces size={13} /> Edit as JSON…</button>
        <button class="nav-item" onclick={() => { setMongoNull(m.r, m.col); menu = null; }}><CircleSlash size={13} /> Set null</button>
        {#if isChanged(m.r, m.col)}
          <button class="nav-item" onclick={() => { setCell(m.r.idx, m.r.row, m.col, m.r.row[m.col]); menu = null; }}><Undo2 size={13} /> Revert cell</button>
        {/if}
      {/if}
      {#if !m.r.isNew}
        {#if isNested(m.r, m.col)}
          <button class="nav-item" onclick={() => { viewCell(m.r, m.col); menu = null; }}><Eye size={13} /> View value</button>
        {/if}
        <button class="nav-item" onclick={() => { copyCell(m.r, m.col); menu = null; }}><Copy size={13} /> Copy value</button>
      {/if}
      {#if edits.on}
        <button class="nav-item {isDeleted(m.r) ? '' : 'text-red-600 dark:text-red-400'}" onclick={() => { deleteRows(Object.keys(selected).length ? Object.keys(selected) : [m.r.key]); menu = null; }}>
          {#if isDeleted(m.r)}<Undo2 size={13} /> Restore document{:else}<Minus size={13} /> Delete document{Object.keys(selected).length > 1 ? 's' : ''}{/if}
        </button>
      {/if}
    {:else}
      {#if !isDeleted(m.r)}
        <button class="nav-item" onclick={() => { startEdit(m.r, m.col); menu = null; }}><Pencil size={13} /> Edit cell</button>
        <button class="nav-item" onclick={() => { write(m.r, m.col, null); menu = null; }}><CircleSlash size={13} /> Set NULL</button>
        {#if isChanged(m.r, m.col)}
          <button class="nav-item" onclick={() => { setCell(m.r.idx, m.r.row, m.col, m.r.row[m.col]); menu = null; }}><Undo2 size={13} /> Revert cell</button>
        {/if}
      {/if}
      <button class="nav-item {isDeleted(m.r) ? '' : 'text-red-600 dark:text-red-400'}" onclick={() => { deleteRows(Object.keys(selected).length ? Object.keys(selected) : [m.r.key]); menu = null; }}>
        {#if isDeleted(m.r)}<Undo2 size={13} /> Restore row{:else}<Minus size={13} /> Delete row{Object.keys(selected).length > 1 ? 's' : ''}{/if}
      </button>
    {/if}
  </div>
{/if}

{#snippet notices(lines: string[])}
  <ul class="space-y-0.5 font-mono text-[11.5px] text-amber-700 dark:text-amber-400" role="status">
    {#each lines as l, i (i)}<li>{l}</li>{/each}
  </ul>
{/snippet}

{#snippet summary(lines: string[])}
  <ol class="space-y-0.5 font-mono text-[11.5px] text-muted">
    {#each lines as l, i (i)}<li class="truncate" title={l}>{l}</li>{/each}
  </ol>
{/snippet}

<EditSaveDialog bind:mode={dialog} {stmts} {conn} transactions={result?.edit?.transactions !== false} {saving} error={edits.error?.message ?? null} onsave={doSave} oncancel={cancelSave} />
<MongoValueDialog bind:edit={valueEdit} />
{#if viewing}
  {@const v = viewing}
  <div class="dialog-backdrop">
    <div class="dialog max-w-xl" role="dialog" aria-labelledby="mongo-view-title">
      <header class="dialog-head">
        <Braces size={15} class="text-faint" />
        <h2 id="mongo-view-title" class="min-w-0 flex-1 truncate">{v.title}</h2>
        <button class="btn btn-ghost -my-1 -mr-1.5 size-7 p-0" onclick={() => (viewing = null)} aria-label="Close"><X size={15} /></button>
      </header>
      <div class="dialog-body max-h-[60vh] overflow-auto">
        {#key v}<JsonTree documents={[v.value]} name={v.title} />{/key}
      </div>
      <footer class="dialog-foot">
        <button class="btn btn-secondary" onclick={() => (viewing = null)}>Close</button>
      </footer>
    </div>
  </div>
{/if}
{#if sqlExport}
  {@const x = sqlExport}
  <ExportSqlDialog columns={x.r.columns} types={x.r.types} engine={conn.engine} schema={x.schema} table={x.table} key={x.key} onexport={(s, t, o) => runSqlExport(x.r, s, t, o)} onclose={() => (sqlExport = null)} />
{/if}
{#if docExport && result?.sql}
  <ExportDocumentsDialog onexport={runDocExport} onclose={() => (docExport = false)} />
{/if}
