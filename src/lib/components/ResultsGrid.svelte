<script module lang="ts">
  import type { QueryResult } from '../api';

  function createGridState() {
    const prefs = $state({
      columnWidths: [] as number[],
      tableWidth: null as number | null,
      selected: {} as Record<string, true>,
      pickedCols: {} as Record<string, true>,
      rowAnchor: -1,
      colAnchor: -1,
      view: 'columns' as 'columns' | 'json',
      scrollTop: 0,
      scrollLeft: 0,
    });
    return prefs;
  }

  // Results of a run can outlive the mounted grid. Closed and replaced results release their state.
  const states = new WeakMap<QueryResult, ReturnType<typeof createGridState>>();
  function stateFor(result: QueryResult) {
    let prefs = states.get(result);
    if (!prefs) states.set(result, prefs = createGridState());
    return prefs;
  }
</script>

<script lang="ts">
  import { untrack, type Snippet } from 'svelte';
  import { ArrowUpDown, ArrowUp, ArrowDown, ArrowUpRight, Check, X, Pencil, Minus, Eye, Undo2, CircleSlash, CircleAlert, CircleCheck, Copy, Braces, Clock, LoaderCircle } from '@lucide/svelte';
  import { api, ApiError, type Cell, type ForeignKeyInfo, type Row, type RunSort, type TypedValue } from '../api';
  import { focusOnMount } from '../actions';
  import { canWrite, isSql } from '../connections.svelte';
  import { pendingCount, resetEdits } from '../editModel';
  import { addRow, buildStatements, confirmDiscard, editBlocker, parseInput, prepareEdit, saveEdits, setCell } from '../edits';
  import { loadMongoCell, mongoColumnType, mongoEditProblem, mongoOriginal, validMongoValue } from '../mongoEdit';
  import { mongoLiteral } from '../mongo/value';
  import { MONGO_WRITABLE_TYPES } from '../api/generated/constants';
  import { loadRedisCell, redisCellProblem, redisDeleteProblem, redisEditProblem, redisOriginal } from '../redisEdit';
  import { cellText, copyValue, csvCell, formatCell, isBytes, isCutOff, moreRows } from '../results';
  import { isBooleanType, isNow, isTemporalType, NOW_VALUE } from '../sql/value';
  import { KEYS } from '../keys';
  import { exportCsv as runExportCsv, exportDocuments as runExportDocuments, exportSql as runExportSql, type ExportMessage } from '../gridExport';
  import { isPlan, reviewPlan } from '../planReview';
  import type { Tab } from '../tabs.svelte';
  import type { Running } from '../run.svelte';
  import EditSaveDialog from './EditSaveDialog.svelte';
  import MongoValueDialog from './MongoValueDialog.svelte';
  import GridCellEditor from './GridCellEditor.svelte';
  import GridToolbar from './GridToolbar.svelte';
  import JsonTree from './JsonTree.svelte';
  import Modal from './Modal.svelte';
  import QueryRunning from './QueryRunning.svelte';
  import { ask } from '../ask.svelte';
  import Menu from './ui/Menu.svelte';

  /** The grid of one tab: its shown result, its grid edits and the API calls scoped to its session. `running`: the tab's run, if any. */
  let {
    tab,
    running,
    oncancel,
    onloadmore,
    onsaved,
    sort = null,
    onsort,
    foreignKeys = [],
    onfollow,
    windowControls,
  }: { tab: Tab; running?: Running; oncancel?: () => void; onloadmore?: () => void; onsaved: () => void; sort?: RunSort | null; onsort?: (sort: RunSort) => void; foreignKeys?: ForeignKeyInfo[]; onfollow?: (fk: ForeignKeyInfo, row: Row) => void; windowControls?: Snippet } = $props();

  const loading = $derived(!!running);
  const result = $derived(tab.result);
  const conn = $derived(tab.resultConn);
  const edits = $derived(tab.edits);

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
  const redisConn = $derived(conn.engine === 'redis');
  const canEditRows = $derived(!redisConn || result?.edit?.pk[0] !== 'key');
  const fileExport = $derived(mongoConn && api.capabilities.fileExport);
  const mongoDocs = $derived(Array.isArray(result?.documents));
  const blocker = $derived(canWrite(conn) ? editBlocker(result, redisConn) : 'Read-only connection: edits are off');
  const count = $derived(pendingCount(edits));

  const emptyGrid = createGridState();
  const grid = $derived(result ? stateFor(result) : emptyGrid);
  const { view, tableWidth, columnWidths, selected, pickedCols } = $derived(grid);

  // Scrolling near the end of the grid loads the next page. Not while edits are pending: a new result drops them.
  let scroller = $state<HTMLDivElement>();
  let viewportHeight = $state(0);
  const canLoadMore = $derived(!!result && moreRows(result) && !!onloadmore && !loading && !count);
  function loadIfAtEnd() {
    const el = scroller;
    if (el?.clientHeight && canLoadMore && el.scrollHeight - el.scrollTop - el.clientHeight < 200) onloadmore?.();
  }
  function onScroll(e: Event & { currentTarget: HTMLDivElement }) {
    if (e.currentTarget !== scroller || !e.currentTarget.clientHeight) return;
    grid.scrollTop = e.currentTarget.scrollTop;
    grid.scrollLeft = e.currentTarget.scrollLeft;
    loadIfAtEnd();
  }

  $effect(() => {
    if (!scroller || !viewportHeight) return;
    const prefs = grid;
    scroller.scrollTo({ top: untrack(() => prefs.scrollTop), left: untrack(() => prefs.scrollLeft) });
  });

  // Also after each page lands: a pane taller than the rows has no scroll event to wait for.
  $effect(() => {
    if (viewportHeight && canLoadMore) untrack(loadIfAtEnd);
  });

  // A new result (or a connection turned read-only) drops edit mode unless the result is still editable.
  $effect(() => {
    if (!blocker) return;
    edits.on = false;
    if (redisConn) {
      editing = null;
      redisText = null;
    }
  });

  // A notice not tied to a row (for example "not editable") goes away by itself; row errors stay until dismissed.
  $effect(() => {
    if (!edits.error || edits.error.key) return;
    const timer = setTimeout(() => (edits.error = null), 4000);
    return () => clearTimeout(timer);
  });

  function sortBy(c: string) {
    if (!result?.sortable || result.summary || loading) return;
    onsort?.({ column: c, columnIndex: result.columns.indexOf(c), ascending: sort?.column === c ? !sort.ascending : true });
  }

  const MIN_COLUMN_WIDTH = 48;
  const MAX_COLUMN_WIDTH = 640;
  const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

  function measuredColumns(handle: HTMLElement) {
    const table = handle.closest('table');
    const header = table?.tHead?.rows[0];
    if (!table || !header || !result) return null;
    if (columnWidths.length === result.columns.length && tableWidth !== null) return { widths: [...columnWidths], width: tableWidth };
    const widths = Array.from(header.cells).slice(1).map((cell) => cell.getBoundingClientRect().width);
    const width = table.getBoundingClientRect().width;
    grid.columnWidths = [...widths];
    grid.tableWidth = width;
    return { widths, width };
  }

  function setColumnWidth(ci: number, value: number, initial: { widths: number[]; width: number }) {
    const width = Math.round(clamp(value, MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH));
    grid.columnWidths[ci] = width;
    grid.tableWidth = initial.width + width - initial.widths[ci];
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

  let exportMsg = $state<ExportMessage | null>(null);
  $effect(() => {
    result;
    exportMsg = null;
  });

  async function exportCsvNow() {
    if (result) exportMsg = await runExportCsv(result, conn, tab.id);
  }

  async function exportDocumentsNow() {
    if (result) exportMsg = await runExportDocuments(result, conn, tab.id);
  }

  async function exportSqlNow() {
    if (result) exportMsg = await runExportSql(result, conn, tab.id, () => result);
  }

  let editing = $state<{ key: string; col: string } | null>(null);
  let draft = $state('');
  let menu = $state.raw<{ anchor: { x: number; y: number } | HTMLElement; cell: HTMLTableCellElement; r: GridRow; col: string } | null>(null);
  let dialog = $state<'preview' | 'save' | null>(null);
  /** The save in flight: its id cancels it, and the dialog shows its busy state. */
  let saving = $state(false);
  let saveId = $state<string | null>(null);
  let inputError = $state<string | null>(null);
  let preparing = $state(false);
  /** Full text of the cell open in the editor when the grid shows it cut off; saving it unchanged is not an edit. */
  let full: { key: string; col: string; text: string | null } | null = null;
  let loadingCell = $state.raw<{ key: string; col: string } | null>(null);
  /** Mongo: the BSON type picked next to the cell editor. */
  let draftType = $state('string');
  let redisText = $state.raw<{ row: GridRow; col: string; initial: string } | null>(null);
  let redisTouched = false;
  /** Mongo: a read-only tree of one nested cell (document or array), opened outside edit mode. */
  let viewing = $state<{ title: string; value: unknown } | null>(null);

  let previous: { result: QueryResult; batch: QueryResult[]; count: number } | null = null;
  $effect.pre(() => {
    const current = result;
    untrack(() => {
      const batch = tab.results;
      // In-place re-runs (pagination, sort, refresh) replace the object in an existing slot.
      // Keep the viewport and widths, but not selection: row indexes can name different rows.
      if (current && previous && previous.batch === batch && previous.count === batch.length && !batch.includes(previous.result)) {
        const old = previous.result;
        if (current.sql === old.sql && current.columns.length === old.columns.length && current.columns.every((c, i) => c === old.columns[i])) {
          const prior = stateFor(old);
          Object.assign(grid, {
            columnWidths: prior.columnWidths, tableWidth: prior.tableWidth, view: prior.view,
            scrollTop: prior.scrollTop, scrollLeft: prior.scrollLeft,
          });
        }
      }
      previous = current ? { result: current, batch, count: batch.length } : null;
      editing = null;
      redisText = null;
      viewing = null;
      menu = null;
      loadingCell = null;
      full = null;
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

  /** A boolean cell: the column type says bool, or the shown value is a boolean. Mongo has its own type list. */
  const isBoolCell = (r: GridRow, c: string, ci: number) => !mongoConn && !redisConn && (typeof value(r, c) === 'boolean' || isBooleanType(result?.types?.[ci] ?? ''));
  /** The cell as a checkbox state; NULL and other values start unchecked. */
  const isTrue = (r: GridRow, c: string) => {
    const v = value(r, c);
    return v === true || v === 1 || v === 'true' || v === '1';
  };

  async function enableEdit(): Promise<boolean> {
    if (!result || preparing) return false;
    if (blocker) {
      edits.error = { message: blocker, key: '' };
      return false;
    }
    const current = result;
    preparing = true;
    const reason = await prepareEdit(current, conn);
    preparing = false;
    if (result !== current || !canWrite(conn)) return false;
    if (reason) {
      edits.error = { message: reason, key: '' };
      return false;
    }
    edits.on = true;
    return true;
  }

  async function toggleEdit() {
    if (edits.on) {
      const current = result;
      if (!await confirmDiscard(edits) || result !== current) return;
      resetEdits(edits);
    } else if (!(await enableEdit())) return;
    grid.selected = {};
    editing = null;
  }

  function write(r: GridRow, c: string, v: Cell) {
    if (r.isNew) {
      const a = edits.added.find((x) => 'a' + x.key === r.key);
      if (a) a.row[c] = v;
    } else setCell(edits, r.idx, r.row, c, v);
  }

  async function startEdit(r: GridRow, c: string, multiline = false) {
    if (isDeleted(r) || loadingCell) return;
    // Outside edit mode a nested cell opens for reading; Edit turns on editing.
    if (!edits.on && isNested(r, c)) return viewCell(r, c);
    const current = result;
    if (!edits.on || !current?.edit) {
      if (preparing || blocker) { await enableEdit(); return; }
      const cell = { key: r.key, col: c };
      loadingCell = cell;
      try {
        if (!(await enableEdit()) || loadingCell !== cell) return;
      } finally {
        if (loadingCell === cell) loadingCell = null;
      }
    }
    if (result !== current || isDeleted(r)) return;
    if (mongoConn) return startMongoEdit(r, c);
    if (redisConn && current) return startRedisEdit(current, r, c, multiline);
    const v = value(r, c);
    if (isBytes(v)) {
      edits.error = { message: 'Binary preview cells are read-only', key: r.key };
      return;
    }
    let text: string | null = v === null || isNow(v) ? '' : typeof v === 'object' ? JSON.stringify(v) : String(v);
    full = null;
    // The grid gets at most 4 KB per cell; load the whole value from the row's primary key.
    if (isCutOff(v) && !r.isNew && !isChanged(r, c)) {
      const pk = current?.edit?.pk;
      if (!current || !pk?.length || !current.sql) {
        edits.error = { message: 'Truncated text cells are read-only', key: r.key };
        return;
      }
      const cell = { key: r.key, col: c };
      loadingCell = cell;
      try {
        text = await api.cellValue({
          request: {
            connId: conn.id,
            database: current.database ?? conn.database,
            sql: current.sql,
            tabId: tab.id,
            pk: Object.fromEntries(pk.map((k) => [k, r.row[k]])),
            column: c,
          },
        });
        if (loadingCell !== cell) return;
      } catch (e) {
        if (loadingCell === cell) edits.error = { message: ApiError.from(e).message, key: r.key };
        return;
      } finally {
        if (loadingCell === cell) loadingCell = null;
      }
      if (result !== current || !edits.on || isDeleted(r)) return;
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
    if (redisConn) {
      // Enter can explicitly set an empty Redis string. Blurring an untouched new cell leaves it unset.
      if (!r.isNew || value(r, c) !== null || redisTouched || !blur) writeRedis(r, c, draft);
      editing = null;
      return;
    }
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

  async function startRedisEdit(current: QueryResult, r: GridRow, c: string, multiline: boolean) {
    if ((editing?.key === r.key && editing.col === c) || redisText) return;
    const bad = redisCellProblem(current, r.idx, c);
    if (bad) return void (edits.error = { message: bad, key: r.key });
    const original = r.row[c];
    let text = r.isNew ? String(original ?? '') : redisOriginal(current, r.idx, c);
    if (text === undefined && !r.isNew && isCutOff(original)) {
      const cell = { key: r.key, col: c };
      loadingCell = cell;
      try {
        text = await loadRedisCell(current, conn, r.idx, c, tab.id);
        if (loadingCell !== cell || result !== current || !edits.on || !canWrite(conn) || isDeleted(r)) return;
        current.loaded ??= {};
        current.loaded[r.idx] = { ...current.loaded[r.idx], [c]: { type: 'string', value: text } };
      } catch (e) {
        if (loadingCell === cell && result === current) edits.error = { message: ApiError.from(e).message, key: r.key };
        return;
      } finally {
        if (loadingCell === cell) loadingCell = null;
      }
    }
    if (text === undefined) {
      edits.error = { message: 'This cell has no original value to edit. Reload the result.', key: r.key };
      return;
    }
    if (isChanged(r, c)) text = String(value(r, c));
    inputError = null;
    redisTouched = false;
    draft = text;
    if (multiline || isCutOff(original) || /[\r\n]/.test(text)) {
      editing = null;
      redisText = { row: r, col: c, initial: text };
    } else editing = { key: r.key, col: c };
  }

  function writeRedis(r: GridRow, c: string, text: string) {
    const current = result;
    if (!current || !edits.on || !canWrite(conn) || isDeleted(r)) return;
    if (r.isNew) write(r, c, text);
    else {
      const original = redisOriginal(current, r.idx, c);
      if (original === undefined) return;
      // A draft equal to the displayed abbreviation is still a change if it differs from the full text.
      setCell(edits, r.idx, { [c]: original }, c, text);
    }
    if (edits.error?.key === r.key) edits.error = null;
  }

  function applyRedisText() {
    const target = redisText;
    if (!target) return;
    // Textareas normalize line endings. Applying without changing the displayed text preserves the exact bytes.
    if ((target.row.isNew && target.row.row[target.col] === null) ||
        (draft !== target.initial.replace(/\r\n?/g, '\n') && draft !== target.initial)) writeRedis(target.row, target.col, draft);
    redisText = null;
  }

  // ---- MongoDB: values keep their BSON type ----

  /** The type a missing (or null) field gets when the user types a value: the column's most frequent writable type. */
  const columnType = (c: string) => mongoColumnType(result, c);

  /** Why the row cannot be edited or deleted: its `_id` has no typed form. */
  const idProblem = (r: GridRow) => (r.isNew ? null : mongoEditProblem(result, r.idx));

  /** The cell as it was loaded, typed. Loads the full value by `_id` when the grid cell does not hold it exactly; null on failure. */
  async function mongoLoad(r: GridRow, c: string): Promise<TypedValue | null> {
    const current = result;
    if (!current?.sql) return null;
    const have = mongoOriginal(current, r.idx, c);
    if (have) return have;
    if (loadingCell) return null;
    const cell = { key: r.key, col: c };
    loadingCell = cell;
    try {
      const v = await loadMongoCell(current, conn, r.idx, c, tab.id);
      return loadingCell === cell && result === current && edits.on ? v : null;
    } catch (e) {
      if (loadingCell === cell) edits.error = { message: ApiError.from(e).message, key: r.key };
      return null;
    } finally {
      if (loadingCell === cell) loadingCell = null;
    }
  }

  /** Stores a typed value; the value as loaded (also as the same mongosh text) clears the change. */
  function writeMongo(r: GridRow, c: string, v: TypedValue) {
    const orig = result ? mongoOriginal(result, r.idx, c) : undefined;
    const same = !!orig && ((orig.type === v.type && orig.value === v.value) || (v.type === 'shell' && orig.type !== 'missing' && mongoLiteral(orig) === v.value));
    setCell(edits, r.idx, r.row, c, same ? r.row[c] : v);
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
    if (type !== 'missing' && !MONGO_WRITABLE_TYPES.includes(type)) return void (edits.error = { message: `${type} values are read-only in the cell editor; use Edit as JSON…`, key: r.key });
    const v = pending ?? (await mongoLoad(r, c));
    if (!v || result !== current || isDeleted(r)) return;
    // The one-line input drops line breaks; the dialog keeps them as `\n` in the string literal.
    if (v.type === 'string' && /[\r\n]/.test(v.value ?? '')) return editJson(r, c);
    draft = v.value ?? '';
    draftType = v.type === 'missing' ? columnType(c) : v.type;
    inputError = null;
    editing = { key: r.key, col: c };
  }

  /** `commit` for Mongo cells: the text must fit the picked type. */
  function commitMongo(r: GridRow, c: string, blur: boolean) {
    const text = draftType === 'string' ? draft : draft.trim();
    if (draftType === 'null') writeMongo(r, c, { type: 'null' });
    // Leaving a NULL or missing cell empty keeps it as it is instead of turning it into ''.
    else if (text === '' && value(r, c) === null) { /* keep NULL */ }
    else if (validMongoValue(draftType, text)) writeMongo(r, c, { type: draftType, value: text });
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
    const text = await ask<string | null>(MongoValueDialog, { title: `Edit ${c}`, text: v.type === 'missing' ? '' : mongoLiteral(v), document: false });
    if (text === null || result !== current) return;
    writeMongo(r, c, { type: 'shell', value: text });
  }

  /** Sets the field to null; the update needs its old value, so a cell the grid cannot hold exactly loads first. */
  async function setMongoNull(r: GridRow, c: string) {
    const bad = idProblem(r);
    if (bad) return void (edits.error = { message: bad, key: r.key });
    if (await mongoLoad(r, c)) writeMongo(r, c, { type: 'null' });
  }

  /** A new document, or the text of one added before: its grid row stays all NULL. */
  async function editDocument(r?: GridRow) {
    if (!result) return;
    const current = result;
    const columns = current.columns;
    const added = r && edits.added.find((a) => 'a' + a.key === r.key);
    const text = await ask<string | null>(MongoValueDialog, {
      title: added ? 'Edit new document' : `Add document to ${current.edit?.table ?? 'the collection'}`,
      text: added?.doc ?? '{\n  \n}',
      document: true,
    });
    if (text === null || result !== current) return;
    if (added) added.doc = text;
    else addRow(edits, Object.fromEntries(columns.map((c) => [c, null])), text);
  }

  // Row-number clicks select rows and header clicks select columns, like DBeaver: plain click picks one,
  // ⌘/Ctrl adds or removes one, Shift extends from the last plain or ⌘ click. ⌘C copies the block.

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

  function pickRow(i: number, e: MouseEvent & { currentTarget: HTMLButtonElement }) {
    grid.selected = pick(selected, rows, i, grid.rowAnchor, e, (r) => r.key);
    if (!e.shiftKey) grid.rowAnchor = i;
    if (!e.shiftKey && !e.metaKey && !e.ctrlKey) grid.pickedCols = {};
    e.currentTarget.focus({ preventScroll: true });
  }

  function pickColumn(ci: number, e: MouseEvent & { currentTarget: HTMLButtonElement }) {
    if (!result) return;
    grid.pickedCols = pick(pickedCols, result.columns, ci, grid.colAnchor, e, (c) => c);
    if (!e.shiftKey) grid.colAnchor = ci;
    if (!e.shiftKey && !e.metaKey && !e.ctrlKey) grid.selected = {};
    e.currentTarget.focus({ preventScroll: true });
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
      .map((r) => cols.map((c) => cellText(value(r, c))).join('\t'))
      .join('\n');
    navigator.clipboard?.writeText(text).catch(() => {});
  }

  /**
   * Copy of a selection that starts in one cell. A selection that runs past the cell (a triple-click in WebKit ends at the next row)
   * copies only that cell's text, with no line break. The grid shows JSON cut at 200 characters; a selection that reaches the cut
   * or runs past the cell copies the whole value from its start.
   */
  function copySelection(e: ClipboardEvent) {
    const sel = getSelection();
    if (!result || !e.clipboardData || sel?.rangeCount !== 1) return;
    const range = sel.getRangeAt(0);
    const start = range.startContainer;
    const el = (start instanceof Element ? start : start.parentElement)?.closest<HTMLElement>('[data-cell]');
    if (!el) return;
    const v = value(rows[Number(el.dataset.row)], result.columns[Number(el.dataset.col)]);
    const shown = el.textContent ?? '';
    const offset = (node: Node, at: number) => {
      const r = document.createRange();
      r.selectNodeContents(el);
      r.setEnd(node, at);
      return r.toString().length;
    };
    const cutJson = !!v && typeof v === 'object' && !isBytes(v) && shown.endsWith('…');
    if (el.contains(range.endContainer) && !(cutJson && offset(range.endContainer, range.endOffset) >= shown.length)) return;
    // NULL, booleans and binary show no text the value starts with; they copy whole, as ⌘C on rows does.
    const whole = v === null || typeof v === 'boolean' || isBytes(v);
    e.preventDefault();
    e.clipboardData.setData('text/plain', whole ? cellText(v) : cellText(v).slice(offset(start, range.startOffset)));
  }

  const redisDeleteBlocker = $derived.by(() => {
    if (!redisConn || !result) return null;
    for (const r of rows) {
      if (!selected[r.key] || isDeleted(r)) continue;
      const bad = redisDeleteProblem(result, r.idx);
      if (bad) return bad;
    }
    return null;
  });

  function deleteRows(keys: string[]) {
    if (redisConn) {
      if (!result || !edits.on || !canWrite(conn)) return;
      const picked = new Set(keys);
      for (const r of rows) {
        if (!picked.has(r.key) || isDeleted(r)) continue;
        const bad = redisDeleteProblem(result, r.idx);
        if (bad) return void (edits.error = { message: bad, key: r.key });
      }
    }
    for (const k of keys) {
      const r = rows.find((x) => x.key === k);
      if (!r) continue;
      const bad = mongoConn ? idProblem(r) : null;
      if (bad) edits.error = { message: bad, key: r.key };
      else if (r.isNew) edits.added = edits.added.filter((a) => 'a' + a.key !== k);
      else if (edits.deleted[r.idx]) delete edits.deleted[r.idx];
      else edits.deleted[r.idx] = true;
    }
    grid.selected = {};
  }

  function newRow() {
    if (!result) return;
    if (redisConn && (!canEditRows || !edits.on || !canWrite(conn))) return;
    if (mongoConn) return editDocument();
    const row: Row = Object.fromEntries(result.columns.map((c) => [c, null]));
    addRow(edits, row);
  }

  function openMenu(e: MouseEvent | KeyboardEvent, cell: HTMLTableCellElement, r: GridRow, c: string) {
    e.preventDefault();
    const point = e instanceof MouseEvent && (e.clientX !== 0 || e.clientY !== 0);
    menu = { anchor: point ? { x: e.clientX, y: e.clientY } : cell, cell, r, col: c };
  }

  function cellKey(e: KeyboardEvent & { currentTarget: HTMLTableCellElement }, r: GridRow, c: string) {
    const cell = e.currentTarget;
    if (e.target !== cell) return;
    if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) return openMenu(e, cell, r, c);
    let next: Element | null | undefined;
    if (e.key === 'ArrowLeft') next = cell.cellIndex > 1 ? cell.previousElementSibling : null;
    else if (e.key === 'ArrowRight') next = cell.nextElementSibling;
    else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
      const row = cell.parentElement;
      const adjacent = e.key === 'ArrowUp' ? row?.previousElementSibling : row?.nextElementSibling;
      if (adjacent instanceof HTMLTableRowElement) next = adjacent.cells[cell.cellIndex];
    }
    if (next instanceof HTMLTableCellElement) {
      e.preventDefault();
      next.focus();
    }
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

  function copyText(text: string) {
    navigator.clipboard?.writeText(text).catch(() => {});
  }

  function copyCell(r: GridRow, c: string) {
    const doc = mongoDocs && !r.isNew && !isChanged(r, c) ? docValue(r, c) : undefined;
    copyText(doc === undefined ? cellText(value(r, c)) : copyValue(doc));
  }

  function contextRows(r: GridRow) {
    return selected[r.key] ? rows.filter((row) => selected[row.key]) : [r];
  }

  function copyRows(picked: GridRow[]) {
    if (!result) return;
    const cols = result.columns;
    copyText(picked.map((r) => cols.map((c) => cellText(value(r, c))).join('\t')).join('\n'));
  }

  function copyCsv() {
    if (!result) return;
    const cols = result.columns;
    copyText([cols.map(csvCell).join(','), ...rows.map((r) => cols.map((c) => csvCell(value(r, c))).join(','))].join('\r\n'));
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

  function previewEdits(mode: 'preview' | 'save') {
    if (redisConn && result) {
      if (!edits.on || blocker) return;
      const problem = redisEditProblem(edits, result);
      if (problem) return void (edits.error = problem);
    }
    dialog = mode;
  }

  async function doSave() {
    if (!result || saving || (redisConn && (blocker || !edits.on))) return;
    saving = true;
    // Closing this tab waits for the save: a reset after it would wipe the edits of the next state.
    edits.saving = true;
    saveId = crypto.randomUUID();
    let ok = false;
    try {
      ok = await saveEdits(result, conn, edits, tab.id, saveId ?? undefined);
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
    if (saveId) api.cancelQuery({ queryId: saveId }).catch(() => {});
  }

  const stmts = $derived(dialog && result && !blocker ? buildStatements(result, conn, edits) : []);

  type Tint = 'error' | 'deleted' | 'new' | 'selected' | 'plain';
  function tintOf(r: GridRow): Tint {
    if (edits.error?.key === r.key) return 'error';
    if (isDeleted(r)) return 'deleted';
    if (r.isNew) return 'new';
    if (selected[r.key] && !hasCols) return 'selected';
    return 'plain';
  }
  const ROW_TINT: Record<Tint, string> = {
    error: 'bg-danger-soft outline outline-1 -outline-offset-1 outline-danger/60',
    deleted: 'bg-danger-soft text-faint line-through decoration-danger/60',
    new: 'bg-ok-soft',
    selected: 'bg-accent/10',
    plain: 'hover:bg-hover',
  };
  /** The row-number cell's tint: its opaque background hides the tint on the `tr`. */
  const NUMBER_TINT: Record<Tint, string> = {
    error: 'bg-danger-soft',
    deleted: 'bg-danger-soft',
    new: 'bg-ok-soft',
    selected: 'bg-accent/10',
    plain: 'group-hover/row:bg-hover',
  };
</script>

{#if running}
  <div class="pointer-events-none absolute inset-x-0 top-0 z-running {windowControls ? 'bottom-sub-toolbar' : 'bottom-0'}">
    <QueryRunning run={running} over={!!result} {oncancel} />
  </div>
{/if}

{#if !result || result.error || result.note}
  <div class="flex h-full flex-col">
    {#if !result}
      <div class="grid min-h-0 flex-1 place-items-center" class:invisible={loading}>
        <div class="text-center">
          <div class="text-base font-medium text-ink">No results yet</div>
          <div class="mt-1 flex items-center justify-center gap-1.5 text-sm text-muted">Write a query and press <kbd class="kbd">{KEYS.run.hint}</kbd></div>
        </div>
      </div>
    {:else if result.error}
      <div class="min-h-0 flex-1 overflow-auto p-4">
        <div class="flex items-start gap-2.5 rounded-lg bg-danger-soft px-3.5 py-3 text-danger" role="alert">
          <CircleAlert size={15} class="mt-px shrink-0" />
          <div class="min-w-0">
            <span class="text-sm font-semibold">Error</span>
            <pre class="mt-1 font-mono text-sm leading-relaxed whitespace-pre-wrap">{result.error}</pre>
          </div>
        </div>
      </div>
    {:else}
      <div class="flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-4">
        <div class="flex items-center gap-2.5">
          <CircleCheck size={15} class="shrink-0 text-ok" />
          <span><span class="sr-only">Succeeded: </span>{result.note} <span class="font-mono text-xs text-faint">· {result.ms} ms</span></span>
        </div>
        {#if result.notices}{@render notices(result.notices)}{/if}
        {#if result.summary}{@render summary(result.summary)}{/if}
      </div>
    {/if}
    {#if windowControls}<GridToolbar {windowControls} />{/if}
  </div>
{:else}
  <div class="flex h-full flex-col">
    {#if result.notices}<div class="border-b border-line px-3 py-1.5">{@render notices(result.notices)}</div>{/if}
    {#if result.summary}<div class="border-b border-line px-3 py-1.5">{@render summary(result.summary)}</div>{/if}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="min-h-0 flex-1 overflow-auto focus-ring" tabindex="-1" bind:this={scroller} bind:clientHeight={viewportHeight} onscroll={onScroll} onkeydown={copyBlock} oncopy={copySelection}>
      {#if mongoDocs && view === 'json'}
        <JsonTree documents={result.documents ?? []} more={moreRows(result)} {loading} onloadmore={onloadmore} />
      {:else}
      <table class="w-full border-separate border-spacing-0 font-mono text-sm {tableWidth === null ? '' : 'table-fixed'}" style:width={tableWidth === null ? undefined : `${tableWidth}px`}>
        <colgroup>
          <col class="w-9" />
          {#each result.columns as _, ci}
            <col style:width={columnWidths[ci] === undefined ? undefined : `${columnWidths[ci]}px`} />
          {/each}
        </colgroup>
        <thead class="sticky top-0 z-sticky">
          <tr>
            <th class="sticky left-0 w-9 border-b border-line bg-surface shadow-[1px_0_0_var(--c-line)]"></th>
            {#each result.columns as c, ci}
              <th class="group/th relative h-8 border-b border-l border-line bg-surface px-2.5 py-0 text-left font-normal" title={result.types?.[ci]}>
                {#if pickedCols[c]}<span class="pointer-events-none absolute inset-0 bg-accent/10"></span>{/if}
                <div class="relative flex w-full items-center gap-1.5">
                  <button class="focus-ring flex min-w-0 flex-1 items-baseline gap-1.5 rounded text-left whitespace-nowrap select-none" title="Select column (⌘ add, ⇧ range)" onclick={(e) => pickColumn(ci, e)}>
                    <span class="font-sans text-sm font-semibold {pickedCols[c] || sort?.column === c ? 'text-accent' : 'text-ink'}">{c}</span>
                    {#if result.types?.[ci]}<span class="th max-w-72 min-w-0 truncate">{result.types?.[ci]}</span>{/if}
                  </button>
                  {#if result.sortable}
                    <button
                      class="focus-ring shrink-0 rounded p-0.5 hover:bg-hover {sort?.column === c ? '' : 'opacity-0 motion-safe:transition-opacity group-hover/th:opacity-100 focus-visible:opacity-100'}"
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
                  class="focus-ring absolute inset-y-0 -right-1 z-sticky w-2 cursor-col-resize hover:bg-accent/25 focus-visible:bg-accent/40"
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
            <tr class="group/row {ROW_TINT[tintOf(r)]}">
              <td
                class="sticky left-0 border-b border-line/60 bg-surface p-0 text-right text-2xs tabular-nums shadow-[1px_0_0_var(--c-line)] select-none {selected[r.key] ? 'font-medium text-ink' : 'text-faint'}"
              >
                <button type="button" class="focus-ring absolute inset-0 w-full cursor-pointer px-2 text-right" aria-label={r.isNew ? 'Select new row' : `Select row ${i + 1}`} aria-pressed={!!selected[r.key]} onclick={(e) => pickRow(i, e)}>
                  <span class="pointer-events-none absolute inset-0 {NUMBER_TINT[tintOf(r)]}"></span><span class="relative">{r.isNew ? '+' : i + 1}</span>
                </button>
              </td>
              {#each result.columns as c, ci}
                {@const v = value(r, c)}
                {@const isEd = editing?.key === r.key && editing.col === c}
                {@const isLoading = loadingCell?.key === r.key && loadingCell.col === c}
                {@const fk = linkOf(r, c)}
                <td
                  title={mongoDocs ? (r.isNew ? 'new document' : isChanged(r, c) ? (edits.changed[r.idx][c] as TypedValue).type : (result.cellTypes?.[r.idx]?.[ci] ?? 'missing')) : undefined}
                  class="focus-ring relative h-list-row {tableWidth === null ? 'max-w-72' : ''} truncate select-none border-b border-l border-line/60 {fk ? 'pr-7 pl-2.5' : 'px-2.5'} {isChanged(r, c) && !isDeleted(r) ? 'bg-warn-soft' : hasCols && inBlock(r, c) ? 'bg-accent/10' : ''} {typeof v === 'number' ? 'text-right' : ''}"
                  aria-busy={isLoading || undefined}
                  tabindex={i === 0 && ci === 0 ? 0 : -1}
                  ondblclick={() => startEdit(r, c)}
                  onclick={(e) => {
                    if (e.metaKey || e.ctrlKey) return;
                    grid.pickedCols = {};
                    grid.selected = {};
                    if (e.target === e.currentTarget || (e.target instanceof Element && e.target.closest('[data-cell]'))) {
                      e.currentTarget.focus({ preventScroll: true });
                    }
                  }}
                  oncontextmenu={(e) => openMenu(e, e.currentTarget, r, c)}
                  onkeydown={(e) => cellKey(e, r, c)}
                >
                  {#if isEd && isBoolCell(r, c, ci)}
                    <!-- WebKit does not focus a clicked checkbox: mousedown would blur it and close the editor before the click toggles it. -->
                    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
                    <label class="absolute inset-0 flex cursor-pointer items-center px-2.5" title="Toggle {c}" onmousedown={(e) => e.preventDefault()}>
                      <input
                        type="checkbox"
                        class="focus-ring size-3.5 cursor-pointer"
                        aria-label="Toggle {c}"
                        checked={isTrue(r, c)}
                        use:focusOnMount
                        onchange={(e) => write(r, c, e.currentTarget.checked)}
                        onkeydown={(e) => {
                          if (e.key === 'Enter') { e.preventDefault(); editing = null; }
                          else if (e.key === 'Escape') { e.stopPropagation(); editing = null; }
                        }}
                        onblur={() => (editing = null)}
                      />
                    </label>
                  {:else if isEd}
                    <GridCellEditor
                      mongo={mongoConn}
                      bind:value={draft}
                      bind:type={draftType}
                      types={MONGO_WRITABLE_TYPES}
                      error={inputError}
                      oncommit={(blur) => commit(r, c, blur)}
                      oncancel={() => (editing = null)}
                      oninput={() => {
                        inputError = null;
                        if (redisConn) redisTouched = true;
                        // Typing into a null cell picks a type that can hold the text.
                        if (mongoConn && draftType === 'null') draftType = columnType(c);
                      }}
                    />
                  {/if}
                  {#if isLoading}
                    <span class="absolute inset-0 flex items-center justify-center bg-field text-accent" role="status">
                      <LoaderCircle size={13} class="motion-safe:animate-spin" aria-hidden="true" /><span class="sr-only">Loading cell…</span>
                    </span>
                  {:else if fk && !isEd}
                    {@const target = (fk.refSchema ? fk.refSchema + '.' : '') + fk.refTable}
                    <button
                      class="focus-ring absolute top-1/2 right-1.5 inline-flex size-4 -translate-y-1/2 items-center justify-center rounded text-faint opacity-60 group-hover/row:opacity-100 focus-visible:opacity-100 hover:bg-accent/15 hover:text-accent"
                      title="Open {target} where {fk.refColumns.map((rc, k) => `${rc} = ${String(r.row[fk.columns[k]])}`).join(' and ')}"
                      aria-label="Open referenced row in {target}"
                      onclick={(e) => { e.stopPropagation(); onfollow?.(fk, r.row); }}
                      ondblclick={(e) => e.stopPropagation()}
                    ><ArrowUpRight size={12} /></button>
                  {/if}
                  <span class:invisible={isEd || isLoading} data-cell data-row={i} data-col={ci}>
                    {#if v === null}
                      <span class="text-xs italic {selected[r.key] ? 'text-muted' : 'text-faint'}">NULL</span>
                    {:else if typeof v === 'boolean'}
                      {#if v}<Check size={13} class="text-ok" />{:else}<X size={13} class="text-faint" />{/if}
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
      <div class="flex items-center gap-2 border-t border-danger/25 bg-danger-soft px-3 py-1.5 text-sm text-danger" role="alert">
        <CircleAlert size={13} class="shrink-0" />
        <span class="sr-only">ERROR</span>
        <span class="min-w-0 flex-1 truncate" title={edits.error.message}>{edits.error.message}</span>
        <button class="btn btn-sm btn-ghost p-1" onclick={() => (edits.error = null)} aria-label="Dismiss"><X size={12} /></button>
      </div>
    {/if}
    <GridToolbar
      {result}
      {edits}
      mongo={mongoConn}
      redis={redisConn}
      {canEditRows}
      deleteBlocker={redisDeleteBlocker}
      {blocker}
      {preparing}
      {loading}
      {view}
      {exportMsg}
      canExportSql={isSql(conn) && api.capabilities.fileExport}
      {fileExport}
      selectedCount={Object.keys(selected).length}
      onedit={toggleEdit}
      onnew={newRow}
      ondelete={() => deleteRows(Object.keys(selected))}
      ondiscard={() => resetEdits(edits, true)}
      onpreview={() => previewEdits('preview')}
      onsave={() => previewEdits('save')}
      onview={(v) => (grid.view = v)}
      onexport={fileExport ? exportDocumentsNow : exportCsvNow}
      onexportsql={exportSqlNow}
      onreview={isPlan(result, conn.engine) ? () => reviewPlan(result, conn.engine) : undefined}
      {windowControls}
    />
  </div>
{/if}

{#if menu}
  {@const { anchor, cell, r, col } = menu}
  {@const picked = contextRows(r)}
  {@const editProblem = redisConn && result ? redisCellProblem(result, r.idx, col) : null}
  {@const deleteProblem = redisConn && result ? picked.reduce<string | null>((bad, row) => bad ?? (isDeleted(row) ? null : redisDeleteProblem(result, row.idx)), null) : null}
  <Menu {anchor} returnFocus={cell} label="Cell actions" onclose={() => (menu = null)}>
    <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => copyCell(r, col)}><Copy size={13} /> Copy cell</button>
    <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => copyRows(picked)}><Copy size={13} /> Copy {picked.length > 1 ? 'rows' : 'row'}</button>
    <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => copyText(rows.map((row) => cellText(value(row, col))).join('\n'))}><Copy size={13} /> Copy column</button>
    <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={copyCsv}><Copy size={13} /> Copy as CSV</button>
    {#if isNested(r, col)}
      <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => viewCell(r, col)}><Eye size={13} /> View value</button>
    {/if}
    {#if edits.on && !blocker}
      <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
      {#if mongoConn && r.isNew}
        <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => editDocument(r)}><Braces size={13} /> Edit document…</button>
      {:else if !isDeleted(r)}
        <button type="button" role="menuitem" tabindex="-1" class="menu-item" disabled={!!editProblem} title={editProblem ?? undefined} onclick={() => startEdit(r, col)}><Pencil size={13} /> Edit cell</button>
        {#if mongoConn}
          <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => editJson(r, col)}><Braces size={13} /> Edit as JSON…</button>
          <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => setMongoNull(r, col)}><CircleSlash size={13} /> Set null</button>
        {:else if redisConn}
          {#if !editProblem}
            <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => startEdit(r, col, true)}><Pencil size={13} /> Edit as multiline text…</button>
          {/if}
        {:else}
          <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => write(r, col, null)}><CircleSlash size={13} /> Set NULL</button>
          {#if isTemporalType(result?.types?.[result.columns.indexOf(col)] ?? '')}
            <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => write(r, col, NOW_VALUE)}><Clock size={13} /> Set NOW()</button>
          {/if}
        {/if}
        {#if isChanged(r, col)}
          <button type="button" role="menuitem" tabindex="-1" class="menu-item" onclick={() => setCell(edits, r.idx, r.row, col, r.row[col])}><Undo2 size={13} /> Revert cell</button>
        {/if}
      {/if}
      {#if canEditRows}
        <button type="button" role="menuitem" tabindex="-1" class="menu-item {isDeleted(r) ? '' : 'nav-item-danger'}" disabled={!!deleteProblem} title={deleteProblem ?? undefined} onclick={() => deleteRows(picked.map((row) => row.key))}>
          {#if isDeleted(r)}<Undo2 size={13} /> Restore {mongoConn ? 'document' : 'row'}
          {:else}<Minus size={13} /> Delete {mongoConn ? 'document' : 'row'}{picked.length > 1 ? 's' : ''}{/if}
        </button>
      {/if}
    {/if}
  </Menu>
{/if}

{#snippet notices(lines: string[])}
  <ul class="space-y-0.5 font-mono text-xs text-warn" role="status">
    {#each lines as l, i (i)}<li>{l}</li>{/each}
  </ul>
{/snippet}

{#snippet summary(lines: string[])}
  <ol class="space-y-0.5 font-mono text-xs text-muted">
    {#each lines as l, i (i)}<li class="truncate" title={l}>{l}</li>{/each}
  </ol>
{/snippet}

<EditSaveDialog bind:mode={dialog} {stmts} {conn} transactions={result?.edit?.transactions !== false} {saving} error={edits.error?.message ?? null} onsave={doSave} oncancel={cancelSave} />
{#if redisText && !blocker && edits.on}
  <Modal title={`Edit ${redisText.col}`} width="max-w-xl" closable onclose={() => (redisText = null)}>
    <textarea class="input h-72 resize-y py-1.5 font-mono text-sm leading-relaxed" data-autofocus aria-label="Cell value" bind:value={draft} autocapitalize="off" autocomplete="off" spellcheck="false"></textarea>
    {#snippet footer()}
      <button class="btn btn-secondary" onclick={() => (redisText = null)}>Cancel</button>
      <button class="btn btn-primary" onclick={applyRedisText}>Apply</button>
    {/snippet}
  </Modal>
{/if}
{#if viewing}
  {@const v = viewing}
  <Modal title={v.title} width="max-w-xl" bodyClass="max-h-[60vh] overflow-auto select-text" closable onclose={() => (viewing = null)}>
    {#snippet head()}<Braces size={15} class="text-faint" />{/snippet}
    {#key v}<JsonTree documents={[v.value]} name={v.title} />{/key}
    {#snippet footer()}
      <button class="btn btn-ghost" onclick={() => copyText(copyValue(v.value))}><Copy size={13} /> Copy value</button>
      <button class="btn btn-secondary" onclick={() => (viewing = null)}>Close</button>
    {/snippet}
  </Modal>
{/if}
