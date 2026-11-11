<script lang="ts">
  import { Database, Search, Settings, ScrollText, Sun, Moon, Plus, Sparkles, PanelRight, PanelLeft, CircleArrowUp, PanelBottom } from '@lucide/svelte';
  import { FileCode, Play, Square, X, LoaderCircle, Check, Undo2, ListOrdered, FileUp, Scissors, Copy, ClipboardPaste, ChevronUp, ChevronDown, Table2, Eye, ScanEye, RotateCw, Save, CopyPlus, WandSparkles } from '@lucide/svelte';
  import { tick, untrack } from 'svelte';
  import { ui, db, active, envTag, canWrite, isSql, structureChanged, type Conn } from './lib/state.svelte';
  import { look, backdropOn, toggleTheme } from './lib/appearance.svelte';
  import { connOf, dbOf, openChatOnTab, type Source as ChatSource } from './lib/chat.svelte';
  import { api, ApiError, type ResultSort, type ForeignKeyInfo, type SnapshotTable, type WriteInfo, type DocumentFormat, type EjsonMode } from './lib/api';
  import { schemas, runMock, statementKind, nsOf, type QueryResult, type Row, type Schema } from './lib/mock';
  import { appendStatement, statementRanges } from './lib/sql/text';
  import { appendMongoStatement, mongoStatementRanges } from './lib/mongo/text';
  import { toResult, toResults, snapshotSchema, snapshotKey, moreRows } from './lib/results';
  import ConnectionTree from './lib/components/ConnectionTree.svelte';
  import ConnectionDialog from './lib/components/ConnectionDialog.svelte';
  import SqlEditor from './lib/components/SqlEditor.svelte';
  import ResultsGrid from './lib/components/ResultsGrid.svelte';
  import ChatPanel from './lib/components/ChatPanel.svelte';
  import SafeguardDialog from './lib/components/SafeguardDialog.svelte';
  import MongoWriteDialog from './lib/components/MongoWriteDialog.svelte';
  import SettingsPage from './lib/components/SettingsPage.svelte';
  import AuditPage from './lib/components/AuditPage.svelte';
  import CommandPalette, { type PaletteTable } from './lib/components/CommandPalette.svelte';
  import DeleteConnectionDialog from './lib/components/DeleteConnectionDialog.svelte';
  import TargetBar from './lib/components/TargetBar.svelte';
  import TableDangerDialog from './lib/components/TableDangerDialog.svelte';
  import TableProperties from './lib/components/TableProperties.svelte';
  import ExportDocumentsDialog from './lib/components/ExportDocumentsDialog.svelte';
  import TableFilterBar from './lib/components/TableFilterBar.svelte';
  import SavedQueries from './lib/components/SavedQueries.svelte';
  import SaveQueryDialog from './lib/components/SaveQueryDialog.svelte';
  import { saved, findSaved, isDirty, suggestName, createSaved, updateSaved, deleteSaved, renameSaved, type SavedQuery, type SaveAsk } from './lib/saved.svelte';
  import { qualified, displayName, selectSql, insertSql, dangerSql, tableDataSql, mongoCollection, type TableAction, type TableRef, type TableFilter, type Danger } from './lib/tableActions';
  import { audit, logQuery } from './lib/audit.svelte';
  import { confirmDiscard, edits, pendingCount, resetEdits } from './lib/edits.svelte';
  import { tabs, activeTab, newTab, selectTab, closeTab, setTabConn, defaultDb, openTableTab, setResults, replaceResult, replaceActiveResult, activateResult, closeResult, clearResults, pendingOf, type Tab } from './lib/tabs.svelte';
  import { manualCommit, refreshStatus, endTx, leaveSession, settlePending, txPrompt } from './lib/tx.svelte';
  import PendingTxDialog from './lib/components/PendingTxDialog.svelte';
  import WhatsNewDialog from './lib/components/WhatsNewDialog.svelte';
  import { updates, checkForUpdates, showWhatsNew } from './lib/updates.svelte';

  const isMock = api.kind === 'mock';
  // The overlay title bar puts the macOS window buttons over the top-left corner.
  const inset = api.kind === 'tauri' && /Mac/.test(navigator.userAgent);
  // Mock mode opens on a sample query, like the prototype.
  if (!tabs.list.length) newTab(isMock ? 'SELECT *\nFROM public.orders\nWHERE status = \'paid\'\nLIMIT 50;' : '');
  // Never undefined: closing the last tab opens a fresh one.
  const tab = $derived(activeTab()!);
  checkForUpdates(true);
  showWhatsNew();

  let guardTarget = $state<Conn | null>(null);
  // The Mongo write confirm: the run waits on the dialog's answer.
  let writeAsk = $state<{ info: WriteInfo; conn: Conn; resolve: (ok: boolean) => void } | null>(null);
  let connDialog = $state(false);
  let connEdit = $state<Conn | null>(null);
  let connDelete = $state<Conn | null>(null);
  let paletteOpen = $state(false);
  let editor = $state<ReturnType<typeof SqlEditor>>();

  // Capture phase so ⌘K wins even while CodeMirror has focus.
  $effect(() => {
    const onkey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        e.stopPropagation();
        if (connDialog || connDelete || danger || guardTarget || writeAsk || txPrompt.ask || saveAsk) return;
        paletteOpen = !paletteOpen;
      } else if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === 's') {
        // ⌘S saves, ⇧⌘S saves as a new query. preventDefault also stops the WebView's "Save page".
        e.preventDefault();
        e.stopPropagation();
        if (connDialog || connDelete || danger || guardTarget || writeAsk || txPrompt.ask || saveAsk || paletteOpen) return;
        saveQuery(e.shiftKey);
      }
    };
    window.addEventListener('keydown', onkey, true);
    return () => window.removeEventListener('keydown', onkey, true);
  });

  async function focusEditor() {
    ui.page = 'query';
    await tick();
    editor?.focus();
  }

  // The WebView's own menu (Reload, Play All Animations) has no use in the app. Text fields and
  // selected text keep it for Cut/Copy/Paste; dev builds keep it for Inspect Element.
  function blockNativeMenu(e: MouseEvent) {
    if (import.meta.env.DEV || e.defaultPrevented) return;
    const t = e.target as HTMLElement;
    if (t.closest('input, textarea, [contenteditable="true"]') || !document.getSelection()?.isCollapsed) return;
    e.preventDefault();
  }

  const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
  let mainEl = $state<HTMLElement>();

  // Pointer capture keeps the moves coming after the pointer leaves the thin handle.
  function drag(e: PointerEvent, move: (dx: number, dy: number) => void) {
    if (e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    const [x, y] = [e.clientX, e.clientY];
    el.setPointerCapture(e.pointerId);
    el.onpointermove = (m) => move(m.clientX - x, m.clientY - y);
    el.onlostpointercapture = () => (el.onpointermove = el.onlostpointercapture = null);
  }

  const setSidebar = (w: number) => (ui.sidebarWidth = Math.round(clamp(w, 160, 480)));
  const setChat = (w: number) => (ui.chatWidth = Math.round(clamp(w, 280, 640)));
  const setSplit = (px: number) => mainEl && (ui.editorSplit = clamp(px / mainEl.clientHeight, 0.1, 0.8));

  function resizeSidebar(e: PointerEvent) {
    const w = ui.sidebarWidth;
    drag(e, (dx) => setSidebar(w + dx));
  }

  // The chat panel is on the right: a drag to the left makes it wider.
  function resizeChat(e: PointerEvent) {
    const w = ui.chatWidth;
    drag(e, (dx) => setChat(w - dx));
  }

  function resizeEditor(e: PointerEvent) {
    const h = ui.editorSplit * (mainEl?.clientHeight ?? 0);
    restoreSplit = null;
    drag(e, (_, dy) => setSplit(h + dy));
  }

  // Arrow keys move a focused splitter by 16 px.
  function keySidebar(e: KeyboardEvent) {
    const d = { ArrowLeft: -16, ArrowRight: 16 }[e.key];
    if (d) { e.preventDefault(); setSidebar(ui.sidebarWidth + d); }
  }

  function keyChat(e: KeyboardEvent) {
    const d = { ArrowLeft: 16, ArrowRight: -16 }[e.key];
    if (d) { e.preventDefault(); setChat(ui.chatWidth + d); }
  }

  function keyEditor(e: KeyboardEvent) {
    const d = { ArrowUp: -16, ArrowDown: 16 }[e.key];
    if (d && mainEl) { e.preventDefault(); restoreSplit = null; setSplit(ui.editorSplit * mainEl.clientHeight + d); }
  }

  // The sash arrows (as in DBeaver): one arrow moves the split to its limit; the other arrow then restores it.
  let restoreSplit: number | null = null;
  function sash(dir: 'up' | 'down') {
    const target = dir === 'up' ? 0.1 : 0.8;
    if (restoreSplit !== null && ui.editorSplit !== target) {
      ui.editorSplit = restoreSplit;
      restoreSplit = null;
      return;
    }
    restoreSplit ??= ui.editorSplit;
    ui.editorSplit = target;
  }

  async function switchConn(c: Conn) {
    if (c.id === conn.id || !confirmDiscard()) return;
    const t = tab;
    if (!(await settlePending([t], 'Switching the connection'))) return;
    leaveSession(t);
    setTabConn(t, c);
    openFirstTable();
  }

  // The session belongs to the old database, like a connection switch.
  async function switchDb(name: string) {
    if (conn.engine === 'sqlite' || name === database || !confirmDiscard()) return;
    const t = tab;
    if (!(await settlePending([t], 'Switching the database'))) return;
    leaveSession(t);
    t.database = name;
    openFirstTable();
  }

  // Double-click, ⌘-click or "View data" in the tree: a table tab with the table's data and properties.
  // A tab that already shows the table comes to the front instead of a second one.
  // `filters`: replace the tab's filters and load again, also when the tab already exists (foreign key links).
  async function openTableView(c: Conn, t: TableRef, filters?: TableFilter[]) {
    // A table opens in its own tab (or picks the open one): the current tab keeps its edits.
    const { tab: nt, created } = openTableTab(c, t);
    if (filters) {
      nt.filters = filters;
      nt.view = 'data';
    }
    ui.page = 'query';
    await tick();
    (tabStrip?.querySelector('[aria-current=page]') ?? tabStrip?.lastElementChild)?.scrollIntoView({ inline: 'nearest' });
    if (created || filters) loadTableData(nt);
  }

  // Foreign keys of each opened table, keyed like `Tab.opened.key`; the Data view links their cells to the referenced row.
  let tableFks = $state<Record<string, ForeignKeyInfo[]>>({});
  const tableId = (c: Conn, t: TableRef) => `${c.id}/${t.database}/${t.schema ?? ''}/${t.name}`;

  /** `force`: load again after a structure change. A failed load leaves the table without links. */
  function loadForeignKeys(c: Conn, t: TableRef, force = false) {
    const key = tableId(c, t);
    if (key in tableFks && !force) return;
    tableFks[key] ??= [];
    api.tableProperties(c.id, t.database, t.schema, t.name).then((p) => (tableFks[key] = p.foreignKeys)).catch(() => {});
  }

  /** Opens the referenced table, filtered to the row that `row` points at. */
  function followForeignKey(c: Conn, from: TableRef, fk: ForeignKeyInfo, row: Row) {
    const values = fk.columns.map((col) => row[col]);
    if (values.some((v) => v === null || typeof v === 'object')) return;
    const ref: TableRef = { database: from.database, schema: fk.refSchema ?? null, name: fk.refTable, kind: 'table' };
    openTableView(c, ref, fk.refColumns.map((column, i) => ({ id: crypto.randomUUID(), column, operator: '=', value: String(values[i]) })));
  }

  /** Runs the table tab's filtered read (SELECT, or find() on MongoDB); also the Refresh button of its Data view. */
  function loadTableData(t: Tab) {
    const ref = t.table!;
    const c = db.connections.find((x) => x.id === t.connId)!;
    // No LIMIT: the run limit pages the rows, so "Load more" can fetch further. A filter value that
    // the read cannot hold as typed stops here, so nothing malformed reaches the server.
    let sql: string;
    try {
      sql = tableDataSql(c.engine, ref, t.filters, t.result?.columns ?? [], t.result?.types ?? []);
    } catch (e) {
      notify(false, e instanceof Error ? e.message : String(e));
      return;
    }
    t.query = sql;
    t.opened = { key: tableId(c, ref), sql };
    // MongoDB has no foreign keys to link.
    if (isSql(c)) loadForeignKeys(c, ref);
    run(undefined, { sql, sort: t.sort });
  }

  // The tabs stay in the title bar on every page; picking one goes back to the query view.
  function pickTab(id: string) {
    // A running save may not see its tab switch: its reset would wipe the next tab's edits.
    if (edits.saving) return;
    // Pending grid edits belong to their tab, so switching never discards them.
    if (id !== tab.id) selectTab(id);
    ui.page = 'query';
  }

  let tabStrip = $state<HTMLDivElement>();
  async function addTab() {
    if (edits.saving) return;
    // A new tab keeps the edits of the tab it leaves: it never asks to discard them.
    newTab();
    ui.page = 'query';
    await tick();
    tabStrip?.lastElementChild?.scrollIntoView({ inline: 'nearest' });
  }

  // Pending edits belong to the active tab's result; other tabs can close without asking.
  async function dropTab(t: Tab) {
    if (edits.saving) return;
    if (isDirty(t) && !confirm(`“${t.title}” has unsaved changes. Close the tab without saving them?`)) return;
    if (t.pending && !confirm(`“${t.title}” has unsaved grid changes. Close the tab without saving them?`)) return;
    if (t.id === tab.id && !confirmDiscard()) return;
    if (!(await settlePending([t], 'Closing this tab')) || !tabs.list.some((x) => x.id === t.id)) return;
    const r = running[t.id];
    if (r) api.cancelQuery(r.id).catch(() => {});
    closeTab(t.id);
    // Every tab may hold a session; the backend ignores tabs without one.
    api.closeTabSession(t.id).catch(() => {});
  }

  let renaming = $state<string | null>(null);
  function rename(t: Tab, title: string) {
    if (renaming !== t.id) return;
    renaming = null;
    if (!title.trim()) return;
    // A linked tab renames its saved query (and every other tab on it).
    if (t.savedId && findSaved(t.savedId)) renameSaved(t.savedId, title);
    else t.title = title.trim();
  }
  const focusSelect = (el: HTMLInputElement) => el.select();

  const conn = $derived(active());
  const hasConn = $derived(db.connections.length > 0);
  // `||`: a tab opened before the connections loaded has no database yet.
  const database = $derived(conn.engine === 'sqlite' ? 'main' : tab.database || conn.database);
  let snapshot = $state<{ key: string; schema: Schema; tables: SnapshotTable[] } | null>(null);
  // The mock serves SQL connections from the prototype's local tables; a mock MongoDB connection
  // goes through the API like a real one, because the mock backend answers its calls.
  const protoMock = $derived(isMock && isSql(conn));
  const schema = $derived(protoMock ? schemas[conn.schema] : snapshot?.key === `${conn.id}/${database}` ? snapshot.schema : {});
  const tables = $derived(Object.keys(schema));
  // Tree highlight follows the shown result; joins, unions, CTEs and aggregates have no single table.
  const selected = $derived.by(() => {
    const { result: r, opened, resultConn } = tab;
    if (!protoMock) return opened && r && !r.error && r.sql === opened.sql ? opened.key : null;
    if (!r?.table || !r.sql || r.error) return null;
    if (/\b(join|union|group\s+by|distinct)\b|^\s*with\b|\bfrom\s+[\w.]+(\s+\w+)?\s*,|\b(count|sum|avg|min|max)\s*\(/i.test(r.sql.replace(/--.*$/gm, ''))) return null;
    return `${resultConn.id}/${nsOf(resultConn.engine, r.table)}/${r.table}`;
  });
  // statementKind reads SQL keywords; a Mongo tab only needs to know whether there is any text to run.
  const kind = $derived(conn.engine === 'mongo' ? (tab.query.trim() ? 'read' : 'empty') : statementKind(tab.query));

  // Autocomplete and palette tables of real connections; failures only leave autocomplete empty.
  $effect(() => {
    if (protoMock || !conn.id) return;
    const key = `${conn.id}/${database}`;
    const engine = conn.engine;
    if (untrack(() => snapshot?.key) === key) return;
    api.schemaSnapshot(conn.id, database).then(
      (s) => { if (`${active().id}/${database}` === key) snapshot = { key, schema: snapshotSchema(s, engine), tables: s.tables }; },
      () => {},
    );
  });

  /**
   * Autocomplete columns of a table the snapshot holds without columns (cut by the column cap), or of a
   * PG table outside `public` written without its schema (only when the name is unique).
   */
  async function columnsOf(path: string[]): Promise<string[]> {
    const c = conn;
    const d = database;
    if (protoMock || snapshot?.key !== `${c.id}/${d}` || path.length > 2) return [];
    const [ns, name] = path.length === 2 ? path : [undefined, path[0]];
    const hits = snapshot.tables.filter((t) => t.name === name && (ns === undefined || t.schema === ns));
    const t = hits.find((t) => !t.schema || t.schema === 'public') ?? (hits.length === 1 ? hits[0] : undefined);
    if (!t) return [];
    if (t.columns.length) return t.columns.map((x) => x.name);
    return (await api.listColumns(c.id, d, t.schema ?? null, t.name)).map((x) => x.name);
  }

  const DEFAULT_LIMIT = 200;
  /** The row count of a trailing `LIMIT n` (also `LIMIT n OFFSET m` and MySQL/SQLite `LIMIT m, n`), or null. */
  function sqlLimit(sql: string): number | null {
    const m = /\blimit\s+(\d+)(?:\s*,\s*(\d+)|\s+offset\s+\d+)?\s*;?$/i.exec(sql.replace(/--.*$/gm, '').replace(/\/\*[\s\S]*?\*\//g, '').trim());
    return m ? Number(m[2] ?? m[1]) : null;
  }
  /** The row count of the last `.limit(<int>)` of the mongosh text, or null. */
  function mongoLimit(sql: string): number | null {
    const all = [...sql.matchAll(/\.limit\s*\(\s*(\d+)\s*\)/g)];
    return all.length ? Number(all.at(-1)![1]) : null;
  }
  // Keyed by tab id; the Run/Cancel button and timer show the active tab's run only.
  // `script`: a file run; `done`/`total` come from the backend while it runs.
  const running = $state<Record<string, { id: string; started: number; script?: { name: string; done: number; total: number } }>>({});
  const busy = $derived(running[tab.id]);
  let now = $state(Date.now());
  $effect(() => {
    if (!busy) return;
    const t = setInterval(() => (now = Date.now()), 250);
    return () => clearInterval(t);
  });

  // Statement kinds that change what the tree lists or a collection's contents.
  const MONGO_STRUCTURE = new Set(['createCollection', 'drop', 'renameCollection', 'createIndex', 'createIndexes', 'dropIndex', 'dropIndexes', 'dropDatabase', 'runCommand']);
  const SQL_STRUCTURE = /^(CREATE|DROP|ALTER|RENAME|TRUNCATE)\b/;

  /** Whether a run changed the schema: the tree and open Properties views then reload. */
  function changesStructure(engine: Conn['engine'], sql: string | undefined, info: WriteInfo | null): boolean {
    if (engine === 'mongo') {
      // An aggregate that writes a collection changes what the tree lists.
      if (sql && /\$out\b|\$merge\b/.test(sql)) return true;
      return !!info?.statements.some((s) => s.read === false && MONGO_STRUCTURE.has(s.kind));
    }
    return !!info?.statements.some((s) => SQL_STRUCTURE.test(s.kind));
  }

  /** Collections a run wrote, with the database each statement ran in (`use` included). */
  function mongoWriteTargets(stmts: { sql: string; isRead: boolean; database?: string }[], fallback: string) {
    const out: { database: string; collection: string }[] = [];
    for (const s of stmts) {
      if (s.isRead) continue;
      const raw = /getCollection\(\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')\s*\)/.exec(s.sql)?.[1] ?? /^db\.([A-Za-z_$][\w$]*)\s*\./.exec(s.sql)?.[1];
      if (!raw) continue;
      const quote = raw[0];
      const collection = quote === '"' || quote === "'" ? raw.slice(1, -1).replace(/\\(["'\\])/g, '$1') : raw;
      out.push({ database: s.database ?? fallback, collection });
    }
    return out;
  }

  /** Reloads the connection's loaded tree nodes and tells open Properties views to reload their data. */
  function treeChanged(c: Conn) {
    tree?.refreshStructure(c);
    structureChanged();
  }

  /**
   * Runs the selection, or the statement at the cursor (as DBeaver's Ctrl+Enter); without an editor,
   * the whole query. `again` re-runs the shown result with a higher limit.
   * `pick` runs the given SQL instead; with `script` (a file name) the toolbar shows statement progress.
   * With `append` the results open as new result tabs after the current ones.
   */
  async function run(again?: { sql: string; limit: number; sort?: ResultSort }, pick?: { sql?: string; script?: string; sort?: ResultSort; append?: boolean }) {
    // The result lands on this tab even if the user switches tabs while it runs.
    const t = tab;
    if (running[t.id] || (!again && !confirmDiscard())) return;
    // A re-run keeps its target: switching or closing result tabs during the await must not
    // move its rows into another Result tab.
    const targetResult = again ? t.result : null;
    const sql = again?.sql ?? pick?.sql ?? (editor ? editor.selection().trim() || editor.statementAt() : t.query);
    const sort = again?.sort ?? pick?.sort;
    if (!again) t.sort = pick?.sort;
    const c = conn;
    // statementKind reads SQL keywords; Mongo only needs to know whether there is any text.
    const mongo = c.engine === 'mongo';
    if (mongo ? !sql.trim() : statementKind(sql) === 'empty') return;
    // A re-run (load more, sort, refresh after save) goes to the database the result came from: a Mongo `use` may have changed it.
    const dbName = (again && targetResult?.database) || database;
    // A LIMIT in the SQL wins over the default page size; the backend caps it at 100,000.
    const limit = again?.limit ?? Math.max((mongo ? mongoLimit(sql) : sqlLimit(sql)) ?? 0, DEFAULT_LIMIT);
    // Every run is classified, so the audit log can tell a write from a read and a schema change
    // refreshes the tree. Production Mongo writes ask first, like the table danger dialogs;
    // unparsable text runs anyway, so the backend's parse error shows in the result.
    let writeInfo: WriteInfo | null = null;
    if (!again) {
      const info = await api.classifyWrite(c.engine, sql, database).catch(() => null);
      writeInfo = info;
      if (mongo && info && c.env === 'production' && canWrite(c) && info.statements.some((s) => s.read === false) && !info.warnings.some((w) => w.code === 'unparsable')) {
        const ok = await new Promise<boolean>((resolve) => (writeAsk = { info, conn: c, resolve }));
        if (!ok) return;
      }
    }
    const id = crypto.randomUUID();
    const started = Date.now();
    running[t.id] = { id, started, script: pick?.script ? { name: pick.script, done: 0, total: 0 } : undefined };
    now = started;
    // A run should show its result, so it opens a hidden grid.
    ui.resultsOpen = true;
    // Polls until the run ends; a poll that fails only leaves the count where it was.
    const poll = pick?.script
      ? setInterval(() => {
          api.queryProgress(id).then((p) => {
            const s = running[t.id]?.id === id ? running[t.id].script : undefined;
            if (p && s) [s.done, s.total] = [p.done, p.total];
          }, () => {});
        }, 300)
      : undefined;
    let r: QueryResult;
    let all: QueryResult[] | undefined;
    let active = 0;
    let runStatements: { sql: string; isRead: boolean; database?: string }[] = [];
    try {
      const queryRun = await api.runQuery({ connId: c.id, database: dbName, sql, queryId: id, limit, tabId: t.id, sort });
      runStatements = queryRun.statements;
      if (again) {
        r = toResult(queryRun, isMock, limit, dbName);
      } else {
        const converted = toResults(queryRun, isMock, limit, dbName);
        all = converted.results;
        active = converted.active;
        r = all[active] ?? { columns: [], rows: [], ms: queryRun.elapsedMs, note: 'Statement ran.' };
      }
    } catch (e) {
      const err = ApiError.from(e);
      r = { columns: [], rows: [], ms: Date.now() - started, error: err.code === 'cancelled' ? (err.detail ? `Query cancelled. ${err.detail}` : 'Query cancelled') : err.message, live: !isMock };
    } finally {
      clearInterval(poll);
      delete running[t.id];
      refreshStatus(t);
    }
    if (again) {
      if (targetResult) replaceResult(t, targetResult, r);
      else replaceActiveResult(t, r);
    } else {
      const fresh = all?.length ? all : [r];
      if (pick?.append) setResults(t, [...t.results, ...fresh], t.results.length + (all?.length ? active : 0));
      else setResults(t, fresh, all?.length ? active : 0);
      t.resultConn = c;
    }
    if (again) {
      t.resultConn = c;
      return;
    }
    // A Mongo write was classified before the dialog; SQL uses statementKind.
    const wrote = mongo ? !!writeInfo?.statements.some((s) => s.read === false) : statementKind(sql) === 'write';
    logQuery({
      kind: wrote ? 'WRITE' : 'READ',
      source: 'You',
      sql: sql.trim(),
      result: r.error ? { status: 'error', message: r.error } : { status: 'ok', rows: r.rows.length },
      ms: r.ms,
    });
    if (!r.error) {
      if (changesStructure(c.engine, sql, writeInfo)) treeChanged(c);
      // A Mongo write into a name the tree does not list can create it: insert, upsert, bulkWrite,
      // or a `use newdb` before the statement. Known collections stay free of a reload.
      else if (mongo && mongoWriteTargets(runStatements, dbName).some((x) => !tree?.hasCollection(c, x.database, x.collection))) treeChanged(c);
    }
  }

  function cancel() {
    if (busy) api.cancelQuery(busy.id).catch(() => {});
  }

  // Editor context menu. It replaces the native menu, so it carries Cut / Copy / Paste too.
  let editorMenu = $state<{ x: number; y: number; sel: boolean } | null>(null);
  let scriptInput = $state<HTMLInputElement>();

  function openEditorMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    editor?.pointAt(e.clientX, e.clientY);
    // Menu is w-64 (256px) and about 340px tall; keep it inside the window.
    editorMenu = { x: Math.max(4, Math.min(e.clientX, innerWidth - 260)), y: Math.max(4, Math.min(e.clientY, innerHeight - 344)), sel: !!editor?.selection() };
  }

  // The action runs first: the menu's `{@const}` reads `editorMenu`, which is null once closed.
  function menuAct(fn: () => void) {
    fn();
    editorMenu = null;
  }

  function copySelection(cut: boolean) {
    const s = editor?.selection();
    if (!s) return;
    navigator.clipboard?.writeText(s).catch(() => {});
    if (cut) editor?.insert('');
  }

  function formatQuery() {
    try {
      editor?.format();
    } catch (e) {
      notify(false, `Cannot format: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  // The file runs as one run, like Run all: one connection, statements in order, stop at the first error.
  async function runScript(e: Event & { currentTarget: HTMLInputElement }) {
    const f = e.currentTarget.files?.[0];
    e.currentTarget.value = ''; // so picking the same file again fires change
    if (f) run(undefined, { sql: await f.text(), script: f.name });
  }

  // Error of the last Commit/Rollback button press, shown on its tab only.
  let txError = $state<{ tab: string; text: string } | null>(null);
  let txBusy = $state(false);
  async function endActive(commit: boolean) {
    const t = tab;
    txBusy = true;
    txError = null;
    try {
      await endTx(t, commit);
    } catch (e) {
      txError = { tab: t.id, text: ApiError.from(e).message };
    } finally {
      txBusy = false;
    }
  }

  // Create and duplicate point the active tab at the new connection.
  async function canSwitch() {
    if (!confirmDiscard()) return false;
    const t = tab;
    if (!(await settlePending([t], 'Switching the connection'))) return false;
    leaveSession(t);
    return true;
  }

  // Saving a connection drops every tab session on it (the backend reconnects with the new settings).
  // A changed target also clears every tab's result and grid edits, so ask for them before the save.
  async function canUpdate(c: Conn, dbChanged: boolean) {
    if (!(await settlePending(tabs.list.filter((t) => t.connId === c.id), 'Saving the connection'))) return false;
    if (!dbChanged) return true;
    const counts = tabs.list.filter((t) => t.connId === c.id).map(pendingOf).filter((n) => n > 0);
    if (!counts.length) return true;
    const n = counts.reduce((a, b) => a + b, 0);
    return confirm(`Discard ${n} unsaved grid change${n === 1 ? '' : 's'} in ${counts.length} tab${counts.length === 1 ? '' : 's'}?`);
  }

  // Tauri: closing the window or menu Quit with a pending transaction asks here first; the backend rolls back
  // whatever is left. A quit during another tab prompt waits for it (Cancel there cancels the quit too).
  $effect(() => {
    if (api.kind !== 'tauri') return;
    let off: (() => void) | undefined;
    let gone = false;
    import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen('sandb://quit-requested', async () => {
          if (edits.saving) return;
          if ((pendingCount() || tabs.list.some((t) => t.pending)) && !confirm('There are unsaved grid changes. Quit without saving them?')) return;
          if (await settlePending(tabs.list, 'Quitting sandb', true)) api.quitApp().catch(() => {});
        }),
      )
      .then((u) => (gone ? u() : (off = u)));
    return () => {
      gone = true;
      off?.();
    };
  });

  // Tauri: the menu's Close Tab (⌘W) closes the active tab. Ignored while a dialog or prompt is open.
  $effect(() => {
    if (api.kind !== 'tauri') return;
    let off: (() => void) | undefined;
    let gone = false;
    import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen('sandb://close-tab', () => {
          if (edits.saving) return;
          if (connDialog || connDelete || danger || guardTarget || writeAsk || txPrompt.ask || paletteOpen) return;
          dropTab(tab);
        }),
      )
      .then((u) => (gone ? u() : (off = u)));
    return () => {
      gone = true;
      off?.();
    };
  });

  // Bridge and mock: best effort only. The browser asks before leaving; on leave, sessions are dropped (rolled back).
  // Every tab, as `status` may be stale; the bridge request uses keepalive so the unload does not cancel it.
  $effect(() => {
    if (api.kind === 'tauri') return;
    const onbefore = (e: BeforeUnloadEvent) => {
      if (tabs.list.some((t) => t.status?.pending) || tabs.list.some((t) => t.pending) || pendingCount()) e.preventDefault();
    };
    const onhide = () => {
      for (const t of tabs.list) api.closeTabSession(t.id).catch(() => {});
    };
    window.addEventListener('beforeunload', onbefore);
    window.addEventListener('pagehide', onhide);
    return () => {
      window.removeEventListener('beforeunload', onbefore);
      window.removeEventListener('pagehide', onhide);
    };
  });

  // Tauri decides a window close in Rust: tell it when grid edits (live or in an inactive tab) are unsaved.
  let unsavedSent: boolean | null = null;
  $effect(() => {
    const unsaved = pendingCount() > 0 || tabs.list.some((t) => t.pending);
    if (unsaved === unsavedSent) return;
    unsavedSent = unsaved;
    api.setUnsavedEdits?.(unsaved).catch(() => {});
  });

  function loadMore() {
    const r = tab.result;
    if (r?.sql && r.limit && moreRows(r)) run({ sql: r.sql, limit: r.limit * 2, sort: tab.table ? tab.sort : r.sort });
  }

  function resultLabel(t: Tab, r: QueryResult, i: number) {
    if (r.title) return r.title;
    return t.results.length > 1 ? `Result ${i + 1}` : (r.table ?? 'Results');
  }

  function resultTip(r: QueryResult, i: number) {
    return r.title ?? r.table ?? r.sql?.replace(/\s+/g, ' ').slice(0, 120) ?? `Result ${i + 1}`;
  }

  /** Switching result tabs drops edit mode, like switching editor tabs. */
  function openResult(t: Tab, i: number) {
    if (t.resultIndex === i) return;
    if (!confirmDiscard()) return;
    activateResult(t, i);
    resetEdits(true);
  }

  /** The X of one result tab; closing the last one hides the results pane. */
  function dropResult(t: Tab, i: number) {
    const active = i === t.resultIndex;
    if (active && !confirmDiscard()) return;
    closeResult(t, i);
    if (active) resetEdits(true);
    if (!t.results.length) ui.resultsOpen = false;
  }

  function sortResult(t: Tab, sort: ResultSort) {
    const r = t.result;
    if (!r?.sql || !r.sortable || r.summary || running[t.id] || !confirmDiscard()) return;
    // The sort belongs to the shown result, not to the editor tab.
    if (t.table) t.sort = sort;
    else r.sort = sort;
    run({ sql: r.sql, limit: r.limit ?? DEFAULT_LIMIT, sort });
  }

  function refreshResult(t: Tab) {
    const r = t.result;
    if (r?.sql) run({ sql: r.sql, limit: r.limit ?? DEFAULT_LIMIT, sort: t.table ? t.sort : r.sort });
  }
  function newConnection() {
    connEdit = null;
    connDialog = true;
  }

  function editConnection(c: Conn) {
    connEdit = c;
    connDialog = true;
  }

  // The copy is saved (and switched to) only when the user clicks Save in the dialog.
  function duplicateConnection(c: Conn) {
    const name = `${c.name} (copy)`;
    connEdit = { ...c, id: name.toLowerCase().replace(/[^a-z0-9]+/g, '-') + '-' + Date.now().toString(36), name, aiWrite: false, copyOf: c.id };
    connDialog = true;
  }

  // The dialog asks about transactions; a changed target makes every result on the connection stale.
  function connEdited(c: Conn, dbChanged: boolean) {
    // The save closed the tab sessions on this connection.
    for (const t of tabs.list) if (t.connId === c.id) refreshStatus(t);
    if (!dbChanged) return;
    const sql = c.id === conn.id ? tab.result?.sql : undefined;
    const dropped = tabs.list.filter((t) => t.connId === c.id && t.pending).length;
    for (const t of tabs.list) {
      if (t.connId !== c.id) continue;
      t.database = defaultDb(c);
      // The rows came from the old target; parked edits and results go with it.
      clearResults(t);
      t.opened = null;
    }
    if (dropped) notify(false, `Unsaved grid edits in ${dropped} tab${dropped === 1 ? '' : 's'} were dropped: the connection target changed.`);
    if (c.id !== conn.id) return;
    if (!protoMock) return openFirstTable();
    if (sql) replaceActiveResult(tab, runMock(sql, schema, conn));
  }

  // Throws on a backend error; DeleteConnectionDialog shows it and keeps the connection.
  async function deleteConnection(c: Conn) {
    const wasActive = c.id === conn.id;
    if (wasActive && !confirmDiscard()) return;
    // One decision for every tab on the connection; Cancel leaves the delete dialog open.
    if (!(await settlePending(tabs.list.filter((t) => t.connId === c.id), 'Deleting the connection'))) return;
    await api.deleteConnection(c.id);
    db.connections.splice(db.connections.findIndex((x) => x.id === c.id), 1);
    connDelete = null;
    // Other tabs on it move too; their results would otherwise stay editable against a deleted connection.
    // With none left, they keep the stale id until the next saved connection claims the active tab.
    const next = db.connections[0];
    for (const t of tabs.list) {
      if (t.connId !== c.id) continue;
      if (next) setTabConn(t, next);
      clearResults(t);
      t.opened = null;
      // The backend dropped its session with the connection.
      t.status = null;
    }
    if (wasActive && next) openFirstTable();
  }

  // Shown at once; on a save error the list goes back to the old order.
  async function reorderConnections(ids: string[]) {
    const prev = db.connections.map((c) => c.id);
    const at = (order: string[], c: Conn) => order.indexOf(c.id) >>> 0; // Unknown ids (-1) go last.
    db.connections.sort((a, b) => at(ids, a) - at(ids, b));
    try {
      await api.reorderConnections(ids);
    } catch (e) {
      db.connections.sort((a, b) => at(prev, a) - at(prev, b));
      notify(false, `Could not save the connection order: ${ApiError.from(e).message}`);
    }
  }

  function openFirstTable() {
    if (protoMock) return openTable(tables[0]);
    // Real databases: nothing runs until the user opens a table or runs a query.
    tab.opened = null;
    clearResults(tab);
  }

  /** Mock mode: runs the table in the active tab. `t`: a table name as the palette lists it. */
  function openTable(t: string) {
    if (!confirmDiscard()) return;
    tab.query = `SELECT *\nFROM ${conn.engine === 'pg' ? nsOf(conn.engine, t) + '.' : ''}${t}\nLIMIT 200;`;
    // The editor text updates after this tick, so the statement goes in as SQL.
    run(undefined, { sql: tab.query });
  }

  /** A palette table: real connections open it in its own table tab, like the tree does. */
  function openPaletteTable(t: string) {
    ui.page = 'query';
    if (protoMock) return openTable(t);
    const found = snapshot?.key === `${conn.id}/${database}` ? snapshot.tables.find((x) => snapshotKey(x, conn.engine) === t) : undefined;
    if (found) openTableView(conn, { database, schema: found.schema ?? null, name: found.name, kind: found.kind });
  }

  // ---- table menu of the connection tree ----
  let tree = $state<ReturnType<typeof ConnectionTree>>();

  // AI writes run inside the chat; the audit log tells us when one succeeded, so a schema change
  // there refreshes the tree too (the seeded demo entries are skipped).
  let auditSeen = untrack(() => audit.entries.at(-1)?.id ?? 0);
  $effect(() => {
    const last = audit.entries.at(-1);
    if (!last || last.id <= auditSeen) return;
    const fresh = audit.entries.filter((e) => e.id > auditSeen);
    auditSeen = last.id;
    for (const e of fresh) {
      if (e.source !== 'AI' || e.kind !== 'WRITE' || e.result.status !== 'ok') continue;
      const c = db.connections.find((x) => x.id === e.connId);
      if (!c) continue;
      void api.classifyWrite(c.engine, e.sql)
        .then((info) => {
          if (changesStructure(c.engine, e.sql, info)) return treeChanged(c);
          if (c.engine !== 'mongo') return;
          const targets = mongoWriteTargets([{ sql: e.sql, isRead: false, database: c.database }], c.database);
          if (targets.some((x) => !tree?.hasCollection(c, x.database, x.collection))) treeChanged(c);
        })
        .catch(() => {});
    }
  });

  // Palette tables: the whole active database (its snapshot), plus every table list the tree has loaded in other databases.
  const paletteTables = $derived.by<PaletteTable[]>(() => {
    const here = `${conn.name} / ${database}`;
    const out: PaletteTable[] = tables.map((t) => ({ key: `${conn.id}/${database}/${t}`, label: t, hint: here, open: () => openPaletteTable(t) }));
    if (protoMock) return out;
    for (const { c, t } of tree?.loadedTables() ?? []) {
      if (c.id === conn.id && t.database === database) continue;
      const open = () => { ui.page = 'query'; openTableView(c, t); };
      // Same naming as the snapshot tables: `public` stays implicit.
      const label = t.schema && t.schema !== 'public' ? displayName(t) : t.name;
      out.push({ key: `${c.id}/${t.database}/${t.schema ?? ''}/${t.name}`, label, hint: `${c.name} / ${t.database}`, open });
    }
    return out;
  });

  let danger = $state<Danger | null>(null);
  // The tree's "Export collection..." dialog: the collection it exports, while the dialog is open.
  let docExport = $state<{ c: Conn; t: TableRef } | null>(null);
  // Outcome of an action that has no other place to show it (count, export, a save, an error). `action`: e.g. Undo.
  let notice = $state<{ ok: boolean; text: string; action?: { label: string; run: () => void } } | null>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  function notify(ok: boolean, text: string, action?: { label: string; run: () => void }) {
    notice = { ok, text, action };
    clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => (notice = null), ok ? (action ? 8000 : 5000) : 10000);
  }

  // ---- saved queries ----
  let saveAsk = $state<SaveAsk | null>(null);
  const savedQ = $derived(findSaved(tab.savedId));
  const tabClean = $derived(!!savedQ && !isDirty(tab));

  /** ⌘S: a linked tab writes to its query at once; any other tab asks for a name. `copy` (⇧⌘S) always asks and saves a new query. */
  function saveQuery(copy = false) {
    const t = tab;
    if (ui.page !== 'query' || t.table || !hasConn) return;
    const q = findSaved(t.savedId);
    if (q && !copy) return updateSaved(t);
    if (!t.query.trim()) return;
    const name = q ? `${q.name} copy` : /^Query \d+$/.test(t.title) ? suggestName(t.query) : t.title;
    saveAsk = { tab: t, name, copy: !!q };
  }

  function confirmSave(a: SaveAsk, name: string) {
    saveAsk = null;
    createSaved(a.tab, name);
    notify(true, `Saved “${name}”`, ui.sidebarOpen && ui.sidebarView === 'queries' ? undefined : { label: 'Show', run: () => { ui.sidebarOpen = true; ui.sidebarView = 'queries'; } });
    focusEditor();
  }

  /** Opens `q` in a new tab on its connection and database; an empty tab there is reused. */
  async function openSaved(q: SavedQuery, andRun = false) {
    // A new tab keeps the current tab's edits; a reused blank tab has no result to edit.
    // A deleted connection: the query opens on the current one.
    const own = db.connections.find((x) => x.id === q.connId);
    const c = own ?? conn;
    const dbName = own && q.database ? q.database : defaultDb(c);
    const cur = tab;
    const blank = !cur.table && !cur.savedId && !cur.query.trim() && !cur.result && !running[cur.id] && cur.connId === c.id && cur.database === dbName;
    const t = blank ? cur : newTab();
    if (!blank) {
      setTabConn(t, c);
      t.database = dbName;
      t.resultConn = c;
    }
    t.query = q.sql;
    t.title = q.name;
    t.savedId = q.id;
    ui.page = 'query';
    await tick();
    tabStrip?.querySelector('[aria-current=page]')?.scrollIntoView({ inline: 'nearest' });
    if (andRun) run(undefined, { sql: t.query });
    else editor?.focus();
  }

  function deleteQuery(q: SavedQuery) {
    notify(true, `Deleted “${q.name}”`, { label: 'Undo', run: deleteSaved(q.id) });
  }

  /** Generated SQL opens in a new tab on the table's connection and database; nothing runs. */
  function openInNewTab(c: Conn, t: TableRef, sql: string) {
    const nt = newTab(sql);
    setTabConn(nt, c);
    if (c.engine !== 'sqlite') nt.database = t.database;
    ui.page = 'query';
  }

  /** Chat SQL opens on the database its step ran on; older steps without one use the active tab's database. */
  function chatTab(sql: string, src?: ChatSource) {
    const nt = newTab(sql);
    const c = src && connOf(src);
    if (src && c) {
      setTabConn(nt, c);
      nt.resultConn = c;
      if (c.engine !== 'sqlite') nt.database = dbOf(src, c);
    }
    return nt;
  }

  /** A Run from a chat code block: a new tab there, or the active editor tab the user picked when it is on the same database. */
  async function chatRun(sql: string, src?: ChatSource, target: 'active' | 'new' = 'new') {
    // Writes are inserted for review, never executed straight from an assistant reply.
    const engine = (src ? connOf(src)?.engine : conn.engine) ?? 'pg';
    let hasWrite: boolean;
    if (engine === 'mongo') {
      // mongosh text: the guard API classifies it; when it cannot answer — or the text does not
      // parse — fail closed to insert-only so nothing runs blind.
      const info = await api.classifyWrite('mongo', sql).catch(() => null);
      hasWrite = !info || info.warnings.some((w) => w.code === 'unparsable') || info.statements.some((s) => s.read === false);
    } else {
      hasWrite = statementRanges(sql, engine).some(([from, to]) => statementKind(sql.slice(from, to)) === 'write');
    }
    if (hasWrite) {
      ui.page = 'query';
      chatInsert(sql, src);
      return;
    }
    const t = tab;
    const same = !src || !connOf(src) || (t.connId === src.connId && t.database === dbOf(src));
    if (target === 'new' || !same) {
      ui.page = 'query';
      chatTab(sql, src);
      run(undefined, { sql });
      return;
    }
    const empty = engine === 'mongo' ? !mongoStatementRanges(sql).length : statementKind(sql) === 'empty';
    if (t.table || running[t.id] || empty || !confirmDiscard()) return;
    ui.page = 'query';
    // Keep the query already in the tab; the new one goes below it as its own statement.
    t.query = engine === 'mongo' ? appendMongoStatement(t.query, sql) : appendStatement(t.query, sql, conn.engine);
    run(undefined, { sql });
  }

  /** Appended to the active editor when it is on the step's database, else into a new tab there. */
  function chatInsert(sql: string, src?: ChatSource) {
    const same = !src || !connOf(src) || (tab.connId === src.connId && tab.database === dbOf(src));
    if (!tab.table && same) tab.query = conn.engine === 'mongo' ? appendMongoStatement(tab.query, sql) : appendStatement(tab.query, sql, conn.engine);
    else chatTab(sql, src);
  }

  async function tableAction(a: TableAction, c: Conn, t: TableRef) {
    const e = c.engine;
    try {
      switch (a) {
        case 'view':
          return openTableView(c, t);
        case 'ddl':
          return openInNewTab(c, t, await api.tableDdl(c.id, t.database, t.schema, t.name));
        case 'select':
        case 'insert': {
          const cols = await api.listColumns(c.id, t.database, t.schema, t.name);
          return openInNewTab(c, t, a === 'select' ? selectSql(e, t, cols) : insertSql(e, t, cols));
        }
        case 'count': {
          const sql = `SELECT count(*) FROM ${qualified(e, t)}`;
          const started = Date.now();
          // No tabId: a read through the pool, outside any tab transaction.
          const r = await api.runQuery({ connId: c.id, database: t.database, sql, queryId: crypto.randomUUID(), limit: 1 });
          logQuery({ kind: 'READ', source: 'You', sql, result: { status: 'ok', rows: 1 }, ms: Date.now() - started, conn: c });
          const n = Number(r.statements[0]?.rows[0]?.[0]);
          return notify(true, `${displayName(t)}: ${Number.isFinite(n) ? n.toLocaleString() : '?'} rows`);
        }
        case 'exportDdl':
        case 'exportSql': {
          const out = await api.exportTableSql(c.id, t.database, t.schema, t.name, a === 'exportSql');
          const what = e === 'mongo' ? 'script' : 'DDL';
          if (!out.cancelled) notify(true, a === 'exportSql' && t.kind === 'table' ? `Exported the ${what} and ${out.rows} rows to ${out.path}` : `Exported the ${what} to ${out.path}`);
          return;
        }
        case 'exportCsv': {
          const out = await api.exportCsv(c.id, t.database, `SELECT * FROM ${qualified(e, t)}`);
          if (!out.cancelled) notify(true, `Exported ${out.rows} rows to ${out.path}`);
          return;
        }
        case 'exportDocuments':
          docExport = { c, t };
          return;
        case 'truncate':
        case 'drop':
          danger = { action: a, c, t };
      }
    } catch (err) {
      notify(false, ApiError.from(err).message);
    }
  }

  // The tree's collection export: the dialog asks for the format, and the read is every document of the collection.
  // Throws on a backend error; the dialog shows it and stays open.
  async function exportCollection(format: DocumentFormat, ejson: EjsonMode) {
    const d = docExport;
    if (!d) return;
    const sql = `${mongoCollection(d.t.name)}.find({})`;
    const out = format === 'csv' ? await api.exportCsv(d.c.id, d.t.database, sql) : await api.exportDocuments(d.c.id, d.t.database, sql, format, ejson);
    if (out.cancelled) return;
    docExport = null;
    notify(true, `Exported ${out.rows} ${format === 'csv' ? 'rows' : 'documents'} to ${out.path}`);
  }

  // Runs through the pool without a tabId, so it commits at once, also on Production (the dialog asks first).
  // Throws on a backend error; the dialog shows it and stays open.
  async function runDanger(d: Danger) {
    const sql = dangerSql(d.c.engine, d.t, d.action);
    const started = Date.now();
    try {
      const r = await api.runQuery({ connId: d.c.id, database: d.t.database, sql, queryId: crypto.randomUUID(), limit: 1 });
      logQuery({ kind: 'WRITE', source: 'You', sql, result: { status: 'ok', rows: r.statements[0]?.affected ?? 0 }, ms: Date.now() - started, conn: d.c });
    } catch (err) {
      logQuery({ kind: 'WRITE', source: 'You', sql, result: { status: 'error', message: ApiError.from(err).message }, ms: Date.now() - started, conn: d.c });
      throw err;
    }
    danger = null;
    tree?.refreshTables(d.c, d.t);
    notify(true, `${d.action === 'drop' ? 'Dropped' : 'Truncated'} ${displayName(d.t)}`);
  }</script>

<!-- Behind everything: the picked image, or nothing (see-through window). The ::after layer tints it with the canvas color. -->
{#if backdropOn()}
  <div class="app-backdrop" aria-hidden="true">
    {#if look.backdrop === 'image'}<div class="app-backdrop-image" style:background-image="url({JSON.stringify(look.image)})"></div>{/if}
  </div>
{/if}

<div class="flex h-full flex-col">
  <!-- Title bar. data-tauri-drag-region acts on the element itself only, so the empty strips drag the window and the buttons stay clickable. -->
  <div class="flex h-10 shrink-0 items-center px-2 select-none" data-tauri-drag-region>
    <div class="flex h-full shrink-0 items-center gap-2 {inset ? 'pl-[76px]' : 'pl-2.5'}" style:width={ui.sidebarOpen ? `${ui.sidebarWidth}px` : undefined} data-tauri-drag-region>
      <!-- The app icon's spark: a horizontal bar and two bars at ±60°. -->
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.75" stroke-linecap="round" class="shrink-0 text-accent" aria-hidden="true"><path d="M5 12h14M8.5 5.938l7 12.124M15.5 5.938l-7 12.124" /></svg>
      <span class="text-[13px] font-semibold tracking-[-0.01em]" data-tauri-drag-region>sandb</span>
      <button class="btn btn-ghost ml-auto size-7 shrink-0 p-0" onclick={() => (ui.sidebarOpen = !ui.sidebarOpen)} title={ui.sidebarOpen ? 'Hide sidebar' : 'Show sidebar'} aria-label="Toggle sidebar" aria-pressed={ui.sidebarOpen}><PanelLeft size={15} /></button>
    </div>
    {#if hasConn}
      <!-- The strip starts at the main card's left edge, so the first tab lines up with the card below. -->
      <div bind:this={tabStrip} class="ml-1.5 flex h-full min-w-0 items-center gap-1 overflow-x-auto pl-0.5 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        {#each tabs.list as t (t.id)}
          {@const on = t.id === tab.id && ui.page === 'query'}
          {@const dirtyT = isDirty(t)}
          {@const prodT = db.connections.find((c) => c.id === t.connId)?.env === 'production'}
          <!-- Production tabs carry a red underline, so the cue shows before the tab is opened. -->
          <div class="group relative flex h-7 shrink-0 items-center rounded-md text-[12.5px] {on ? 'tab-on font-medium text-ink' : 'text-muted hover:bg-hover hover:text-ink'}">
            {#if prodT}<span class="pointer-events-none absolute inset-x-1.5 bottom-0 h-0.5 rounded-full bg-red-600 dark:bg-red-500" aria-hidden="true"></span>{/if}
            {#if renaming === t.id}
              <span class="flex items-center gap-2 pl-2.5"><FileCode size={13} class="text-faint" />
                <input class="w-28 bg-transparent outline-none" value={t.title} aria-label="Tab name" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" use:focusSelect onblur={(e) => rename(t, e.currentTarget.value)} onkeydown={(e) => { if (e.key === 'Enter') e.currentTarget.blur(); else if (e.key === 'Escape') renaming = null; }} />
              </span>
            {:else}
              <button class="flex h-full items-center gap-2 pl-2.5" aria-current={t.id === tab.id ? 'page' : undefined} onclick={() => pickTab(t.id)} ondblclick={() => (renaming = t.id)} title="{prodT ? 'Production · ' : ''}Double-click to rename">
                {#if t.table}<Table2 size={13} class={on ? 'text-accent' : 'text-faint'} />{:else}<FileCode size={13} class={on ? 'text-accent' : 'text-faint'} />{/if} <span class="max-w-40 truncate">{t.title}</span>{#if prodT}<span class="sr-only">, production</span>{/if}
                {#if running[t.id] && !on}<LoaderCircle size={12} class="animate-spin text-faint" />{/if}
                {#if t.status?.pending}<span class="size-1.5 rounded-full bg-amber-500" title="Transaction open"></span>{/if}
              </button>
            {/if}
            <!-- Unsaved changes to a saved query: a dot, as in code editors; it turns into the X on hover. -->
            <button class="mr-1 ml-1.5 grid size-[18px] place-items-center rounded text-faint hover:bg-hover hover:text-ink {on || dirtyT ? '' : 'opacity-0 group-hover:opacity-100 focus-visible:opacity-100'}" title={dirtyT ? 'Unsaved changes · close tab' : 'Close tab'} aria-label="Close {t.title}{dirtyT ? ' (unsaved changes)' : ''}" onclick={() => dropTab(t)}>
              {#if dirtyT}<span class="size-2 rounded-full bg-accent group-hover:hidden"></span><X size={12} class="hidden group-hover:block" />{:else}<X size={12} />{/if}
            </button>
          </div>
        {/each}
      </div>
      <button class="ml-1 grid size-7 shrink-0 place-items-center rounded-md text-faint hover:bg-hover hover:text-ink" title="New query" aria-label="New query" onclick={addTab}><Plus size={15} /></button>
    {/if}
    <div class="h-full min-w-6 flex-1" data-tauri-drag-region></div>
    {#if updates.status === 'available' || updates.status === 'restart'}
      <button class="mr-1 flex h-7 items-center gap-1.5 rounded-md px-2 text-[12px] font-medium text-amber-600 hover:bg-hover dark:text-amber-400" title="sandb v{updates.next} is available" onclick={() => { ui.settingsSection = 'updates'; ui.page = 'settings'; }}><CircleArrowUp size={14} /> Update</button>
    {/if}
    <button class="btn btn-ghost size-7 p-0 {ui.chatOpen ? 'bg-hover text-ink' : ''}" onclick={() => (ui.chatOpen = !ui.chatOpen)} title="Toggle AI chat" aria-label="Toggle AI chat"><PanelRight size={15} /></button>
  </div>

  <div class="relative flex min-h-0 flex-1 gap-2 px-2 pb-2">
  <!-- Sidebar: sits on the canvas, so it reads as one piece with the title bar.
       Hidden, not unmounted, so the tree keeps its expanded nodes. -->
  <aside class="flex shrink-0 flex-col overflow-hidden" style:width="{ui.sidebarWidth}px" hidden={!ui.sidebarOpen}>
    <div class="pt-1 pb-2">
      <button class="flex h-7 w-full items-center gap-2 rounded-md bg-hover px-2.5 text-muted hover:bg-active hover:text-ink" onclick={() => (paletteOpen = true)}>
        <Search size={13} /> <span class="flex-1 text-left">Quick search</span>
        <kbd class="text-[11px]">⌘K</kbd>
      </button>
    </div>

    <div class="seg mb-1.5 flex w-full" role="tablist" aria-label="Sidebar">
      <button class="seg-item flex-1 px-1.5 {ui.sidebarView === 'connections' ? 'seg-on' : ''}" role="tab" aria-selected={ui.sidebarView === 'connections'} onclick={() => (ui.sidebarView = 'connections')}>Connections</button>
      <button class="seg-item flex flex-1 items-center justify-center gap-1.5 px-1.5 {ui.sidebarView === 'queries' ? 'seg-on' : ''}" role="tab" aria-selected={ui.sidebarView === 'queries'} onclick={() => (ui.sidebarView = 'queries')}>Queries {#if saved.list.length}<span class="text-[11px] font-normal text-faint tabular-nums">{saved.list.length}</span>{/if}</button>
    </div>

    <!-- Both stay mounted: the tree keeps its expanded nodes, the list its filter. -->
    <nav class="-mx-1 flex-1 overflow-auto px-1 pb-2" hidden={ui.sidebarView !== 'connections'}>
      <ConnectionTree bind:this={tree} {selected} onopen={openTableView} onnew={newConnection} onedit={editConnection} onduplicate={duplicateConnection} ondelete={(c) => (connDelete = c)} ontable={tableAction} onreorder={reorderConnections} />
    </nav>
    <nav class="-mx-1 flex-1 overflow-auto px-1 pb-2" hidden={ui.sidebarView !== 'queries'} aria-label="Saved queries">
      <SavedQueries activeId={ui.page === 'query' ? (savedQ?.id ?? null) : null} onopen={(q) => openSaved(q)} onrun={(q) => openSaved(q, true)} ondelete={deleteQuery} />
    </nav>

    <div class="flex items-center gap-0.5 pt-1">
      <button class="nav-item flex-1 {ui.page === 'settings' ? 'nav-item-active' : ''}" onclick={() => (ui.page = ui.page === 'settings' ? 'query' : 'settings')}><Settings size={14} /> Settings</button>
      <button class="nav-item size-[26px] justify-center p-0 {ui.page === 'audit' ? 'nav-item-active' : ''}" title="Audit log" aria-label="Audit log" onclick={() => (ui.page = ui.page === 'audit' ? 'query' : 'audit')}><ScrollText size={14} /></button>
      <button class="btn btn-ghost size-[26px] p-0" title="Toggle theme" onclick={toggleTheme}>
        {#if ui.dark}<Sun size={14} />{:else}<Moon size={14} />{/if}
      </button>
    </div>
  </aside>

  <!-- Fills the gap between the sidebar and the card: the negative margins cancel its share of the flex gap.
       A focusable separator is the ARIA window-splitter pattern; Svelte counts every separator as static. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="-mx-2 w-2 shrink-0 cursor-col-resize rounded-full outline-none hover:bg-accent/25 focus-visible:bg-accent/40" role="separator" aria-orientation="vertical" aria-label="Resize sidebar" aria-valuenow={ui.sidebarWidth} aria-valuemin={160} aria-valuemax={480} tabindex="0" hidden={!ui.sidebarOpen} onpointerdown={resizeSidebar} onkeydown={keySidebar}></div>

  <!-- Main -->
  <!-- `overflow-clip`, not `overflow-hidden`: a too-wide row (the grid footer with the chat open) must never scroll
       the pane when a dialog hands focus back to a button, which cut off the left edge. -->
  <main bind:this={mainEl} class="card relative flex min-w-[300px] flex-1 flex-col overflow-clip">
    <!-- Production work gets a red top edge on every view of the tab: table data, properties and the editor. -->
    {#if ui.page === 'query' && hasConn && conn.env === 'production'}<div class="pointer-events-none absolute inset-x-0 top-0 z-10 h-0.5 bg-red-600 dark:bg-red-500" aria-hidden="true"></div>{/if}
    {#if ui.page === 'settings'}
      <SettingsPage onEnableWrites={(c) => (guardTarget = c)} />
    {:else if ui.page === 'audit'}
      <AuditPage />
    {:else if !hasConn}
      <div class="grid flex-1 place-items-center p-6 text-center">
        <div class="max-w-xs space-y-2">
          <div class="mx-auto grid size-9 place-items-center rounded-lg bg-accent/12 text-accent"><Database size={17} /></div>
          <p class="font-medium">{db.loading ? 'Loading connections…' : 'No connections yet'}</p>
          {#if db.loadError}<p class="text-[12px] text-red-600 dark:text-red-400" role="alert">{db.loadError}</p>{/if}
          {#if !db.loading}<button class="btn btn-primary mx-auto" onclick={newConnection}><Plus size={13} /> New connection</button>{/if}
        </div>
      </div>
    {:else if tab.table}
    {@const t = tab.table}
    <div class="flex h-11 shrink-0 items-center gap-2 px-3">
      {#if t.kind === 'view'}<Eye size={14} class="shrink-0 text-accent" />{:else if t.kind === 'matview'}<ScanEye size={14} class="shrink-0 text-accent" />{:else}<Table2 size={14} class="shrink-0 text-accent" />{/if}
      <span class="min-w-0 truncate text-[13px] font-semibold" title={displayName(t)}>{displayName(t)}</span>
      {#if conn.env === 'production'}<span class="tag shrink-0 {envTag.production}">production</span>{/if}
      <span class="hidden min-w-0 truncate text-[12px] text-faint md:inline">{conn.name} / {database}</span>
      <!-- Collections show their properties too: fields, indexes, validator, options, stats and the script. -->
      <div class="seg ml-2 shrink-0" role="tablist" aria-label="Table view">
        <button class="seg-item {tab.view === 'data' ? 'seg-on' : ''}" role="tab" aria-selected={tab.view === 'data'} onclick={() => (tab.view = 'data')}>Data</button>
        <button class="seg-item {tab.view === 'properties' ? 'seg-on' : ''}" role="tab" aria-selected={tab.view === 'properties'} onclick={() => (tab.view = 'properties')}>Properties</button>
      </div>
      <div class="flex-1"></div>
      {#if tab.view === 'data'}
        {#if busy}
          <span class="font-mono text-[11.5px] text-muted tabular-nums">{((now - busy.started) / 1000).toFixed(1)} s</span>
          <button class="btn btn-secondary" onclick={cancel} title="Cancel the running query"><Square size={11} class="fill-current" /> Cancel</button>
        {:else}
          <button class="btn btn-ghost" onclick={() => loadTableData(tab)} title="Load the rows again"><RotateCw size={13} /> Refresh</button>
        {/if}
      {/if}
    </div>
    {#if tab.view === 'data'}
      {@const tableKey = conn.engine === 'pg' && t.schema && t.schema !== 'public' ? `${t.schema}.${t.name}` : t.name}
      {@const columns = tab.result?.columns.length ? tab.result.columns : (schema[tableKey] ?? [])}
      <div class="flex min-h-0 flex-1 flex-col border-t border-line">
        <TableFilterBar {columns} types={tab.result?.types ?? []} bind:filters={tab.filters} loading={!!busy} onapply={() => loadTableData(tab)} />
        <div class="relative min-h-0 flex-1">
          <ResultsGrid result={tab.result} conn={tab.resultConn} {schema} loading={!!busy} sort={tab.sort ?? null} onsort={(s) => sortResult(tab, s)} onloadmore={loadMore} onsaved={() => { treeChanged(tab.resultConn); refreshResult(tab); }} foreignKeys={tableFks[tab.opened?.key ?? '']} onfollow={(fk, row) => followForeignKey(conn, t, fk, row)} />
        </div>
      </div>
    {:else}
      <div class="flex min-h-0 flex-1 border-t border-line">
        {#key tab.id}<TableProperties {conn} table={t} onchanged={() => { loadForeignKeys(conn, t, true); tree?.refreshTables(conn, t); }} onopen={(r) => openTableView(conn, r)} />{/key}
      </div>
    {/if}
    {:else}
    <div class="flex h-11 shrink-0 items-center gap-1.5 px-2.5">
      <TargetBar {conn} {database} onconn={switchConn} ondb={switchDb} />
      <!-- The container query sits on this group, not on the toolbar: in WebKit before 18.2 a size container
           also traps TargetBar's fixed menu backdrop. A size container has no content width, so min-w keeps
           room for the icon-only buttons; labels show only when the group has more room. -->
      <div class="@container flex flex-1 items-center gap-1.5 {manualCommit(conn) && conn.engine !== 'mongo' ? 'min-w-[88px]' : 'min-w-0'}">
      {#if manualCommit(conn) && conn.engine !== 'mongo'}
        {@const st = tab.status}
        <span class="mx-1 h-4 w-px shrink-0 bg-line"></span>
        <!-- An aborted transaction counts as pending; Rollback is the way out of it. Disabled buttons do not show tooltips reliably, so the wrappers carry the title. -->
        <span class="shrink-0" title="Commit the tab's open transaction"><button class="btn btn-ghost px-2 @[16rem]:px-2.5" onclick={() => endActive(true)} disabled={!st?.pending || st.aborted || !!busy || txBusy} title="Commit the tab's open transaction" aria-label="Commit"><Check size={14} /> <span class="hidden @[16rem]:inline">Commit</span></button></span>
        <span class="shrink-0" title="Roll back the tab's open transaction"><button class="btn btn-ghost px-2 @[16rem]:px-2.5" onclick={() => endActive(false)} disabled={!(st?.pending || st?.aborted) || !!busy || txBusy} title="Roll back the tab's open transaction" aria-label="Rollback"><Undo2 size={14} /> <span class="hidden @[16rem]:inline">Rollback</span></button></span>
        {#if st?.pending}
          <span class="flex min-w-0 items-center gap-1.5 text-[12px] font-medium text-amber-700 dark:text-amber-400" role="status" title="Transaction open"><span class="size-1.5 shrink-0 rounded-full bg-amber-500"></span> <span class="truncate">Transaction open</span></span>
        {/if}
        {#if txError?.tab === tab.id}<span class="min-w-0 truncate text-[12px] text-red-700 dark:text-red-400" role="alert">{txError.text}</span>{/if}
      {/if}
      </div>
      <!-- statementKind reads SQL keywords, so a Mongo tab has no Read/Write cue. -->
      {#if conn.engine !== 'mongo'}
        {#if kind === 'read'}
          <span class="hidden shrink-0 px-1 text-[12px] font-medium text-emerald-700 lg:inline dark:text-emerald-400" role="status" title="This query only reads data">Read</span>
        {:else if kind === 'write'}
          <span class="shrink-0 px-1 text-[12px] font-medium text-amber-700 dark:text-amber-400" role="status" title="This query changes data">Write</span>
        {/if}
      {/if}
      <!-- A saved, unchanged tab shows "Saved"; ⌘S then has nothing to write. -->
      <button class="btn btn-ghost shrink-0 px-2 xl:px-2.5" onclick={() => saveQuery()} disabled={!savedQ && kind === 'empty'} title={savedQ ? (tabClean ? `“${savedQ.name}” is saved · ⇧⌘S saves a copy` : `Save changes to “${savedQ.name}” (⌘S)`) : 'Save query (⌘S)'} aria-label={tabClean ? 'Saved' : 'Save query'}>
        {#if tabClean}<Check size={14} class="text-emerald-600 dark:text-emerald-400" />{:else}<Save size={14} />{/if}
        <span class="hidden xl:inline">{tabClean ? 'Saved' : 'Save'}</span>
      </button>
      <!-- Opens the chat on this tab's database. With the chat already open, narrow toolbars drop it. -->
      <button class="btn btn-ghost shrink-0 px-2 2xl:px-2.5 {ui.chatOpen ? 'hidden lg:inline-flex' : ''}" onclick={openChatOnTab} title="Explain with AI"><Sparkles size={14} /> <span class="hidden 2xl:inline">Explain with AI</span></button>
      {#if busy}
        {#if busy.script}
          {@const s = busy.script}
          <!-- Total is 0 until the first poll answers. -->
          <span class="flex min-w-0 items-center gap-2 text-[12px] text-muted" role="status" title="Running {s.name}">
            <span class="max-w-40 truncate">{s.name}</span>
            <span class="h-1 w-24 shrink-0 overflow-hidden rounded-full bg-hover" role="progressbar" aria-label="Script progress" aria-valuenow={s.done} aria-valuemin={0} aria-valuemax={s.total || undefined}>
              <span class="block h-full rounded-full bg-accent transition-[width]" style:width="{s.total ? (s.done / s.total) * 100 : 0}%"></span>
            </span>
            <span class="shrink-0 font-mono text-[11.5px] tabular-nums">{s.total ? `${s.done}/${s.total}` : '…'}</span>
          </span>
        {/if}
        <span class="font-mono text-[11.5px] text-muted tabular-nums">{((now - busy.started) / 1000).toFixed(1)} s</span>
        <button class="btn btn-secondary" onclick={cancel} title="Cancel the running query"><Square size={11} class="fill-current" /> Cancel</button>
      {:else}
        <button class="btn btn-primary pr-2" onclick={() => run()} disabled={kind === 'empty'} title="Run the selection, or the statement at the cursor"><Play size={11} class="fill-current" /> Run <kbd class="ml-0.5 hidden font-sans text-[11px] font-normal lg:inline" aria-hidden="true">⌘↵</kbd></button>
      {/if}
    </div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="min-h-28 overflow-hidden {ui.resultsOpen ? 'shrink-0' : 'flex-1'}" style:height={ui.resultsOpen ? `${ui.editorSplit * 100}%` : undefined} oncontextmenu={openEditorMenu}>
      <!-- Keyed so undo never crosses into another tab's text. -->
      {#key tab.id}
        <SqlEditor bind:this={editor} bind:value={tab.query} onrun={() => run()} onrunnew={() => run(undefined, { append: true })} onrunall={() => run(undefined, { sql: tab.query })} onformat={formatQuery} {schema} {columnsOf} dialect={conn.engine} />
      {/key}
    </div>
    <input bind:this={scriptInput} type="file" accept=".sql,text/plain" class="hidden" onchange={runScript} />

    <!-- Hidden, not unmounted, so the grid keeps its scroll, selection and edit mode. -->
    <div class="contents" hidden={!ui.resultsOpen}>
      <div class="relative z-10 -my-1 h-2 shrink-0">
        <!-- Overlaps the editor and grid by 4 px each, so the grab area is wider than the border line. -->
        <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
        <div class="absolute inset-0 cursor-row-resize outline-none hover:bg-accent/25 focus-visible:bg-accent/40" role="separator" aria-orientation="horizontal" aria-label="Resize editor" aria-valuenow={Math.round(ui.editorSplit * 100)} aria-valuemin={10} aria-valuemax={80} tabindex="0" onpointerdown={resizeEditor} onkeydown={keyEditor}></div>
        <div class="absolute top-1/2 left-1/2 flex -translate-x-1/2 -translate-y-1/2 overflow-hidden rounded-md border border-line bg-surface">
          <button class="grid h-4 w-5 place-items-center text-faint hover:bg-hover hover:text-ink" title="Maximize results / restore" aria-label="Move split up" onclick={() => sash('up')}><ChevronUp size={12} /></button>
          <button class="grid h-4 w-5 place-items-center text-faint hover:bg-hover hover:text-ink" title="Maximize editor / restore" aria-label="Move split down" onclick={() => sash('down')}><ChevronDown size={12} /></button>
        </div>
      </div>

      <div class="flex min-h-0 flex-1 flex-col border-t border-line">
        <!-- Result tab strip (as in DBeaver): one tab per grid result; its X closes that result. -->
        {#if tab.results.length}
        <div class="flex h-8 shrink-0 items-end gap-0.5 overflow-x-auto border-b border-line px-1.5" role="tablist" aria-label="Results">
            {#each tab.results as r, i (i)}
              {@const on = tab.resultIndex === i}
              <div
                class="-mb-px flex h-7 shrink-0 items-center gap-1.5 rounded-t-md border border-b-0 pr-1 pl-2.5 text-[12.5px] font-medium {on ? 'border-line bg-surface text-ink' : 'border-transparent text-muted hover:bg-hover hover:text-ink'}"
                role="tab"
                aria-selected={on}
                title={resultTip(r, i)}
                tabindex="0"
                onclick={() => openResult(tab, i)}
                onkeydown={(e) => { if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); openResult(tab, i); } }}
              >
                {#if r.title === 'Messages'}<ScrollText size={13} class="shrink-0 text-accent" />{:else}<Table2 size={13} class="shrink-0 text-accent" />{/if}
                <span class="max-w-40 truncate">{resultLabel(tab, r, i)}</span>
                <button class="ml-1 grid size-[18px] place-items-center rounded text-faint hover:bg-hover hover:text-ink" title="Close result" aria-label="Close result" onclick={(e) => { e.stopPropagation(); dropResult(tab, i); }}><X size={12} /></button>
              </div>
            {/each}
        </div>
        {/if}
        <div class="relative min-h-0 flex-1">
          {#key `${tab.id}:${tab.resultIndex}:${tab.result?.sql ?? ''}:${tab.result?.title ?? ''}`}
            <ResultsGrid result={tab.result} conn={tab.resultConn} {schema} loading={!!busy} sort={tab.result?.sort ?? null} onsort={(s) => sortResult(tab, s)} onloadmore={loadMore} onsaved={() => { treeChanged(tab.resultConn); refreshResult(tab); }} />
          {/key}
        </div>
      </div>
    </div>

    <footer class="flex h-8 shrink-0 items-center gap-2 border-t border-line px-2 text-[12px] text-muted">
      <button class="btn h-6 px-2 text-[12px] {ui.resultsOpen ? 'bg-hover text-ink' : 'btn-ghost'}" aria-pressed={ui.resultsOpen} title={ui.resultsOpen ? 'Hide results' : 'Show results'} onclick={() => (ui.resultsOpen = !ui.resultsOpen)}><PanelBottom size={13} /> Results</button>
      {#if !ui.resultsOpen && tab.result}
        <span class="text-faint tabular-nums">{tab.result.rows.length}{tab.result.truncated ? '+' : ''} rows · {tab.result.ms} ms</span>
      {/if}
    </footer>
    {/if}
  </main>

  {#if ui.chatOpen}
    <!-- Below 880 px the panel floats over the main card, so it has no gap to drag. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div class="-mx-2 hidden w-2 shrink-0 cursor-col-resize rounded-full outline-none hover:bg-accent/25 focus-visible:bg-accent/40 min-[880px]:block" role="separator" aria-orientation="vertical" aria-label="Resize assistant" aria-valuenow={ui.chatWidth} aria-valuemin={280} aria-valuemax={640} tabindex="0" onpointerdown={resizeChat} onkeydown={keyChat}></div>
    <ChatPanel {tables} activeBusy={!!busy} onEnableWrites={(c) => (guardTarget = c)} onInsertSql={chatInsert} onRunSql={chatRun} />
  {/if}
  </div>
</div>
<svelte:window onclick={() => (editorMenu = null)} oncontextmenu={(e) => { editorMenu = null; blockNativeMenu(e); }} onkeydown={(e) => { if (e.key === 'Escape') editorMenu = null; }} />
{#if editorMenu}
  {@const m = editorMenu}
  <div class="popover fixed z-40 w-64 p-1" style="left:{m.x}px;top:{m.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    <button class="nav-item" role="menuitem" disabled={!!busy} onclick={() => menuAct(() => run(undefined, { sql: m.sel ? editor!.selection() : editor!.statementAt() }))}><Play size={13} /> <span class="flex-1">{m.sel ? 'Run selection' : 'Run statement at cursor'}</span><kbd class="kbd">⌘↵</kbd></button>
    <button class="nav-item" role="menuitem" disabled={!!busy || kind === 'empty'} onclick={() => menuAct(() => run(undefined, { sql: tab.query }))}><ListOrdered size={13} /> <span class="flex-1">Run all statements</span><kbd class="kbd">⌥X</kbd></button>
    <button class="nav-item" role="menuitem" disabled={!!busy} onclick={() => menuAct(() => scriptInput?.click())}><FileUp size={13} /> Run SQL script from file…</button>
    <button class="nav-item" role="menuitem" disabled={kind === 'empty'} onclick={() => menuAct(formatQuery)}><WandSparkles size={13} /> <span class="flex-1">{m.sel ? 'Format selection' : 'Format query'}</span><kbd class="kbd">⇧⌘F</kbd></button>
    <div class="mx-2 my-1 h-px bg-line"></div>
    <button class="nav-item" role="menuitem" disabled={!savedQ && kind === 'empty'} onclick={() => menuAct(() => saveQuery())}><Save size={13} /> <span class="flex-1">{savedQ ? 'Save changes' : 'Save query…'}</span><kbd class="kbd">⌘S</kbd></button>
    <button class="nav-item" role="menuitem" disabled={kind === 'empty'} onclick={() => menuAct(() => saveQuery(true))}><CopyPlus size={13} /> <span class="flex-1">Save as new query…</span><kbd class="kbd">⇧⌘S</kbd></button>
    <div class="mx-2 my-1 h-px bg-line"></div>
    <button class="nav-item" role="menuitem" disabled={!m.sel} onclick={() => menuAct(() => copySelection(true))}><Scissors size={13} /> Cut</button>
    <button class="nav-item" role="menuitem" disabled={!m.sel} onclick={() => menuAct(() => copySelection(false))}><Copy size={13} /> Copy</button>
    <button class="nav-item" role="menuitem" onclick={() => menuAct(() => navigator.clipboard?.readText().then((s) => editor?.insert(s), () => {}))}><ClipboardPaste size={13} /> Paste</button>
    <button class="nav-item" role="menuitem" onclick={() => menuAct(() => editor?.selectAll())}><span class="size-[13px]"></span> Select all</button>
  </div>
{/if}
<SafeguardDialog bind:target={guardTarget} />
<MongoWriteDialog bind:ask={writeAsk} />
<ConnectionDialog bind:open={connDialog} conn={connEdit} canswitch={canSwitch} canupdate={canUpdate} onsaved={openFirstTable} onedited={connEdited} />
<DeleteConnectionDialog bind:target={connDelete} ondelete={deleteConnection} />
<TableDangerDialog bind:target={danger} onconfirm={runDanger} />
{#if docExport}
  <ExportDocumentsDialog onexport={exportCollection} onclose={() => (docExport = null)} />
{/if}
<SaveQueryDialog bind:ask={saveAsk} onsave={confirmSave} />
<WhatsNewDialog />
{#if notice}
  {@const n = notice}
  <div class="popover fixed bottom-3 left-3 z-50 flex max-w-md items-start gap-2 px-3 py-2 text-[12.5px] {n.ok ? 'text-ink' : 'text-red-700 dark:text-red-300'}" role={n.ok ? 'status' : 'alert'}>
    <span class="min-w-0 flex-1 break-words">{n.text}</span>
    {#if n.action}
      {@const a = n.action}
      <!-- Run first: `a` reads `notice`, which is null once cleared. -->
      <button class="-my-0.5 h-6 shrink-0 rounded px-1.5 text-[12.5px] font-medium text-accent hover:bg-hover" onclick={() => { a.run(); notice = null; }}>{a.label}</button>
    {/if}
    <button class="grid size-5 shrink-0 place-items-center rounded text-faint hover:bg-hover hover:text-ink" aria-label="Dismiss" onclick={() => (notice = null)}><X size={12} /></button>
  </div>
{/if}
<CommandPalette
  bind:open={paletteOpen}
  tables={paletteTables}
  onfocuseditor={focusEditor}
  onrun={() => { if (kind === 'empty') return; ui.page = 'query'; run(); }}
  ontogglechat={() => (ui.chatOpen = !ui.chatOpen)}
  ontogglesidebar={() => (ui.sidebarOpen = !ui.sidebarOpen)}
  onpage={(p) => (ui.page = p)}
  ontoggletheme={toggleTheme}
  onnewconnection={newConnection}
  onswitch={(c) => { ui.page = 'query'; switchConn(c); }}
  onsave={() => saveQuery()}
  onopensaved={(q) => openSaved(q)}
/>
<!-- Last, so it stacks above the connection and delete dialogs that wait on it. -->
<PendingTxDialog />
