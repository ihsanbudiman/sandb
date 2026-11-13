<script lang="ts">
  import { Database, Search, Settings, ScrollText, Sun, Moon, Plus, Sparkles, PanelRight, PanelLeft, CircleArrowUp, PanelBottom } from '@lucide/svelte';
  import { FileCode, Play, Square, X, LoaderCircle, Check, Undo2, ListOrdered, FileUp, Scissors, Copy, ClipboardPaste, ChevronUp, ChevronDown, Table2, Eye, ScanEye, RotateCw, Save, CopyPlus, WandSparkles } from '@lucide/svelte';
  import { tick } from 'svelte';
  import { db, envTag, duplicateDraft, sameSource, setAiWrite, dbOf, defaultDb, type Conn } from './lib/connections.svelte';
  import { ui } from './lib/state.svelte';
  import { look, backdropOn, toggleTheme } from './lib/appearance.svelte';
  import { catalog, completionColumns, forget as forgetCatalog, loadSnapshot, loadedTables, properties, snapshot, structureChanged } from './lib/catalog.svelte';
  import { openChatOnTab } from './lib/chat.svelte';
  import type { Source as ChatSource } from './lib/connections.svelte';
  import { api, ApiError } from './lib/api';
  import { appendStatement } from './lib/sql/text';
  import { appendMongoStatement } from './lib/mongo/text';
  import { snapshotSchema } from './lib/results';
  import { confirmDiscard } from './lib/edits';
  import { tabs, activeTab, active, newTab, selectTab, closeTab, setTabConn, openTableTab, openSqlTab, clearResults, activateResult, closeResult, pendingOf, tabDatabase, isPlaceholderTitle, type Tab } from './lib/tabs.svelte';
  import { saved, findSaved, isDirty, suggestName, createSaved, updateSaved, deleteSaved, renameSaved, type SavedQuery } from './lib/saved.svelte';
  import { displayName, selectSql, insertSql, type TableAction, type TableFilter, type TableRef } from './lib/tableActions';
  import { tableKey, tableIdOf, tableLabel } from './lib/tableId';
  import { runs, clock, run, cancel, cancelRun, loadTableData, loadMore, sortResult, refreshResult, countRows, exportTable, exportCollection, askDanger } from './lib/run.svelte';
  import { manualCommit, refreshStatus, leaveSession, settlePending, txButtons, endActive } from './lib/tx.svelte';
  import { ask } from './lib/ask.svelte';
  import { overlays, overlayOpen } from './lib/overlays.svelte';
  import { notify } from './lib/notice.svelte';
  import { KEYS, isKey } from './lib/keys';
  import { classify, statements } from './lib/writeCheck';
  import ConnectionTree from './lib/components/ConnectionTree.svelte';
  import ConnectionDialog from './lib/components/ConnectionDialog.svelte';
  import SqlEditor from './lib/components/SqlEditor.svelte';
  import type { EditorCommands } from './lib/components/SqlEditor.svelte';
  import ResultsGrid from './lib/components/ResultsGrid.svelte';
  import ChatPanel from './lib/components/ChatPanel.svelte';
  import SafeguardDialog from './lib/components/SafeguardDialog.svelte';
  import SettingsPage from './lib/components/SettingsPage.svelte';
  import AuditPage from './lib/components/AuditPage.svelte';
  import CommandPalette, { type PaletteTable } from './lib/components/CommandPalette.svelte';
  import DeleteConnectionDialog from './lib/components/DeleteConnectionDialog.svelte';
  import TargetBar from './lib/components/TargetBar.svelte';
  import TableProperties from './lib/components/TableProperties.svelte';
  import TableFilterBar from './lib/components/TableFilterBar.svelte';
  import SavedQueries from './lib/components/SavedQueries.svelte';
  import SaveQueryDialog from './lib/components/SaveQueryDialog.svelte';
  import NoticeToast from './lib/components/NoticeToast.svelte';
  import WhatsNewDialog from './lib/components/WhatsNewDialog.svelte';
  import AskHost from './lib/components/AskHost.svelte';
  import { updates, checkForUpdates, showWhatsNew } from './lib/updates.svelte';

  // The overlay title bar puts the macOS window buttons over the top-left corner.
  const inset = api.capabilities.windowEffects && /Mac/.test(navigator.userAgent);
  if (!tabs.list.length) newTab('');
  // Never undefined: closing the last tab opens a fresh one.
  const tab = $derived(activeTab()!);
  checkForUpdates(true);
  showWhatsNew();

  let editor = $state<{ commands(): EditorCommands }>();
  let scriptInput = $state<HTMLInputElement>();

  // Capture phase so ⌘K wins even while CodeMirror has focus.
  $effect(() => {
    const onkey = (e: KeyboardEvent) => {
      if (isKey(e, KEYS.palette.combo)) {
        e.preventDefault();
        e.stopPropagation();
        if (overlayOpen()) return;
        overlays.palette = !overlays.palette;
      } else if (!e.altKey && isKey(e, KEYS.save.combo)) {
        // ⌘S saves, ⇧⌘S saves as a new query. preventDefault also stops the WebView's "Save page".
        e.preventDefault();
        e.stopPropagation();
        if (overlayOpen()) return;
        saveQuery(e.shiftKey);
      }
    };
    window.addEventListener('keydown', onkey, true);
    return () => window.removeEventListener('keydown', onkey, true);
  });

  async function focusEditor() {
    ui.page = 'query';
    await tick();
    editor?.commands().focus();
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
    if (c.id === conn.id || !confirmDiscard(tab.edits)) return;
    const t = tab;
    if (!(await settlePending([t], 'Switching the connection'))) return;
    leaveSession(t);
    setTabConn(t, c);
    openFirstTable();
  }

  // The session belongs to the old database, like a connection switch.
  async function switchDb(name: string) {
    if (conn.engine === 'sqlite' || name === database || !confirmDiscard(tab.edits)) return;
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

  /** Opens the referenced table, filtered to the row that `row` points at. */
  function followForeignKey(c: Conn, from: TableRef, fk: { columns: string[]; refSchema?: string | null; refTable: string; refColumns: string[] }, row: Record<string, unknown>) {
    const values = fk.columns.map((col) => row[col]);
    if (values.some((v) => v === null || typeof v === 'object')) return;
    const ref: TableRef = { database: from.database, schema: fk.refSchema ?? null, name: fk.refTable, kind: 'table' };
    openTableView(c, ref, fk.refColumns.map((column, i) => ({ id: crypto.randomUUID(), column, operator: '=', value: String(values[i]) })));
  }

  // The tabs stay in the title bar on every page; picking one goes back to the query view.
  function pickTab(id: string) {
    if (id !== tab.id) selectTab(id);
    ui.page = 'query';
  }

  let tabStrip = $state<HTMLDivElement>();
  async function addTab() {
    // A new tab keeps the edits of the tab it leaves: it never asks to discard them.
    newTab();
    ui.page = 'query';
    await tick();
    tabStrip?.lastElementChild?.scrollIntoView({ inline: 'nearest' });
  }

  // Pending edits belong to the active tab's result; other tabs can close without asking.
  async function dropTab(t: Tab) {
    if (t.edits.saving) return;
    if (isDirty(t) && !confirm(`“${t.title}” has unsaved changes. Close the tab without saving them?`)) return;
    if (t.id !== tab.id && pendingOf(t) && !confirm(`“${t.title}” has unsaved grid changes. Close the tab without saving them?`)) return;
    if (t.id === tab.id && !confirmDiscard(t.edits)) return;
    if (!(await settlePending([t], 'Closing this tab')) || !tabs.list.some((x) => x.id === t.id)) return;
    cancelRun(t);
    closeTab(t.id);
    leaveSession(t);
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
  const database = $derived(tabDatabase(tab, conn));
  // Autocomplete and palette tables of the active database; the catalog loads them once per database.
  const snap = $derived(snapshot(conn.id, database));
  const schema = $derived(snapshotSchema(snap?.status === 'ok' ? snap.items : null));
  const tables = $derived(Object.keys(schema));
  // Tree highlight follows the shown result; joins, unions, CTEs and aggregates have no single table.
  const selected = $derived.by(() => {
    const { result: r, opened } = tab;
    return opened && r && !r.error && r.sql === opened.sql ? tableKey(opened.id) : null;
  });
  // SQL: the statements decide the Run/Save cues. A Mongo tab only needs to know whether there is any text to run.
  const split = $derived(statements(conn.engine, tab.query));
  const kind = $derived(conn.engine === 'mongo' ? (tab.query.trim() ? 'read' : 'empty') : split.empty ? 'empty' : split.write ? 'write' : 'read');
  const busy = $derived(runs[tab.id]);
  const now = $derived(clock.now);
  // Foreign keys of the table a tab shows; the grid links their cells to the referenced row.
  const tableFks = $derived(tab.opened ? (properties(tab.opened.id.connId, tab.opened.id.database, tab.opened.id.schema, tab.opened.id.name)?.items?.foreignKeys ?? []) : []);

  $effect(() => {
    // A structure change or a connection pointing elsewhere drops the snapshot; load it again.
    catalog.rev;
    if (!conn.id) return;
    void loadSnapshot(conn.id, database);
  });

  /** Autocomplete columns beyond the snapshot; the catalog answers from its own caches. */
  const columnsOf = (path: string[]) => completionColumns(conn.id, database, path);

  /** The SQL a plain Run executes: the selection, or the statement at the cursor, or the whole query. */
  function currentSql(): string {
    return editor ? editor.commands().selection().trim() || editor.commands().statementAt() : tab.query;
  }

  // Editor context menu. It replaces the native menu, so it carries Cut / Copy / Paste too.
  let editorMenu = $state<{ x: number; y: number; sel: boolean } | null>(null);

  function openEditorMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    editor?.commands().pointAt(e.clientX, e.clientY);
    // Menu is w-64 (256px) and about 340px tall; keep it inside the window.
    editorMenu = { x: Math.max(4, Math.min(e.clientX, innerWidth - 260)), y: Math.max(4, Math.min(e.clientY, innerHeight - 344)), sel: !!editor?.commands().selection() };
  }

  // The action runs first: the menu's `{@const}` reads `editorMenu`, which is null once closed.
  function menuAct(fn: () => void) {
    fn();
    editorMenu = null;
  }

  function copySelection(cut: boolean) {
    const s = editor?.commands().selection();
    if (!s) return;
    navigator.clipboard?.writeText(s).catch(() => {});
    if (cut) editor?.commands().insert('');
  }

  function formatQuery() {
    try {
      editor?.commands().format();
    } catch (e) {
      notify(false, `Cannot format: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  // The file runs as one run, like Run all: one connection, statements in order, stop at the first error.
  async function runScript(e: Event & { currentTarget: HTMLInputElement }) {
    const f = e.currentTarget.files?.[0];
    e.currentTarget.value = ''; // so picking the same file again fires change
    if (f) run({ sql: await f.text(), script: f.name });
  }

  // Create and duplicate point the active tab at the new connection.
  async function canSwitch() {
    if (!confirmDiscard(tab.edits)) return false;
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
    if (!api.capabilities.quitPrompt) return;
    let off: (() => void) | undefined;
    let gone = false;
    import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen('sandb://quit-requested', async () => {
          if (tabs.list.some((t) => t.edits.saving)) return;
          if ((tabs.list.some((t) => pendingOf(t)) || tabs.list.some((t) => t.status?.pending)) && !confirm('There are unsaved grid changes. Quit without saving them?')) return;
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
    if (!api.capabilities.nativeMenu) return;
    let off: (() => void) | undefined;
    let gone = false;
    import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen('sandb://close-tab', () => {
          if (overlayOpen()) return;
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
    if (!api.capabilities.pageUnload) return;
    const onbefore = (e: BeforeUnloadEvent) => {
      if (tabs.list.some((t) => t.status?.pending) || tabs.list.some((t) => pendingOf(t))) e.preventDefault();
    };
    const onhide = () => {
      for (const t of tabs.list) api.closeTabSession({ tabId: t.id }).catch(() => {});
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
    const unsaved = tabs.list.some((t) => pendingOf(t));
    if (unsaved === unsavedSent) return;
    unsavedSent = unsaved;
    api.setUnsavedEdits(unsaved).catch(() => {});
  });

  /** Switching result tabs drops edit mode, like switching editor tabs. */
  function openResult(t: Tab, i: number) {
    if (t.resultIndex === i) return;
    if (!confirmDiscard(t.edits)) return;
    activateResult(t, i);
  }

  /** The X of one result tab; closing the last one hides the results pane. */
  function dropResult(t: Tab, i: number) {
    const active = i === t.resultIndex;
    if (active && !confirmDiscard(t.edits)) return;
    closeResult(t, i);
    if (!t.results.length) ui.resultsOpen = false;
  }

  function resultLabel(t: Tab, r: { title?: string }, i: number) {
    if (r.title) return r.title;
    return t.results.length > 1 ? `Result ${i + 1}` : 'Results';
  }

  function resultTip(r: { title?: string; sql?: string }, i: number) {
    return r.title ?? r.sql?.replace(/\s+/g, ' ').slice(0, 120) ?? `Result ${i + 1}`;
  }

  function newConnection() {
    connEdit = null;
    overlays.connection = true;
  }

  function editConnection(c: Conn) {
    connEdit = c;
    overlays.connection = true;
  }

  // The copy is saved (and switched to) only when the user clicks Save in the dialog.
  function duplicateConnection(c: Conn) {
    connEdit = duplicateDraft(c);
    overlays.connection = true;
  }
  let connEdit = $state<Conn | null>(null);

  // The dialog asks about transactions; a changed target makes every result on the connection stale.
  function connEdited(c: Conn, dbChanged: boolean) {
    // The save closed the tab sessions on this connection.
    for (const t of tabs.list) if (t.connId === c.id) refreshStatus(t);
    if (!dbChanged) return;
    const dropped = tabs.list.filter((t) => t.connId === c.id && pendingOf(t)).length;
    for (const t of tabs.list) {
      if (t.connId !== c.id) continue;
      t.database = defaultDb(c);
      // The rows came from the old target; edits and results go with it.
      clearResults(t);
      t.opened = null;
    }
    if (dropped) notify(false, `Unsaved grid edits in ${dropped} tab${dropped === 1 ? '' : 's'} were dropped: the connection target changed.`);
    if (c.id === conn.id) openFirstTable();
  }

  /** Throws on a backend error; the delete dialog shows it and keeps the connection. `false`: nothing was deleted. */
  async function deleteConnection(c: Conn): Promise<boolean> {
    const wasActive = c.id === conn.id;
    if (wasActive && !confirmDiscard(tab.edits)) return false;
    // One decision for every tab on the connection; Cancel leaves the delete dialog open.
    if (!(await settlePending(tabs.list.filter((t) => t.connId === c.id), 'Deleting the connection'))) return false;
    await api.deleteConnection({ id: c.id });
    db.connections.splice(db.connections.findIndex((x) => x.id === c.id), 1);
    forgetCatalog(c);
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
    return true;
  }

  async function enableWrites(c: Conn) {
    if (await ask<boolean>(SafeguardDialog, { conn: c })) await setAiWrite(c, true);
  }

  async function askDeleteConnection(c: Conn) {
    await ask<boolean>(DeleteConnectionDialog, { conn: c, run: () => deleteConnection(c) });
  }

  // Shown at once; on a save error the list goes back to the old order.
  async function reorderConnections(ids: string[]) {
    const prev = db.connections.map((c) => c.id);
    const at = (order: string[], c: Conn) => order.indexOf(c.id) >>> 0; // Unknown ids (-1) go last.
    db.connections.sort((a, b) => at(ids, a) - at(ids, b));
    try {
      await api.reorderConnections({ ids });
    } catch (e) {
      db.connections.sort((a, b) => at(prev, a) - at(prev, b));
      notify(false, `Could not save the connection order: ${ApiError.from(e).message}`);
    }
  }

  function openFirstTable() {
    // Nothing runs until the user opens a table or runs a query.
    tab.opened = null;
    clearResults(tab);
  }

  // Palette tables: the whole active database (its snapshot), plus every table list the tree has loaded in other databases.
  const paletteTables = $derived.by<PaletteTable[]>(() => {
    const here = `${conn.name} / ${database}`;
    const out: PaletteTable[] = [];
    const seen = new Set<string>();
    for (const t of snap?.status === 'ok' ? (snap.items?.tables ?? []) : []) {
      const label = tableLabel(t);
      if (seen.has(label)) continue;
      seen.add(label);
      out.push({ key: `${conn.id}/${database}/${label}`, label, hint: here, open: () => openTableView(conn, { database, schema: t.schema ?? null, name: t.name, kind: t.kind }) });
    }
    for (const { c, t } of loadedTables()) {
      if (c.id === conn.id && t.database === database) continue;
      const open = () => { ui.page = 'query'; openTableView(c, t); };
      out.push({ key: tableKey(tableIdOf(c.id, t)), label: tableLabel(t), hint: `${c.name} / ${t.database}`, open });
    }
    return out;
  });

  // ---- saved queries ----
  const savedQ = $derived(findSaved(tab.savedId));
  const tabClean = $derived(!!savedQ && !isDirty(tab));

  /** ⌘S: a linked tab writes to its query at once; any other tab asks for a name. `copy` (⇧⌘S) always asks and saves a new query. */
  async function saveQuery(copy = false) {
    const t = tab;
    if (ui.page !== 'query' || t.table || !hasConn) return;
    const q = findSaved(t.savedId);
    if (q && !copy) return updateSaved(t);
    if (!t.query.trim()) return;
    const name = q ? `${q.name} copy` : isPlaceholderTitle(t.title) ? suggestName(t.query) : t.title;
    const chosen = await ask<string | null>(SaveQueryDialog, {
      name,
      copy: !!q,
      conn: db.connections.find((c) => c.id === t.connId),
      database: t.database,
      lines: t.query.split('\n').length,
    });
    if (!chosen) return;
    createSaved(t, chosen);
    notify(true, `Saved “${chosen}”`, ui.sidebarOpen && ui.sidebarView === 'queries' ? undefined : { label: 'Show', run: () => { ui.sidebarOpen = true; ui.sidebarView = 'queries'; } });
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
    const blank = !cur.table && !cur.savedId && !cur.query.trim() && !cur.result && !runs[cur.id] && cur.connId === c.id && cur.database === dbName;
    const t = blank ? cur : newTab();
    if (!blank) {
      setTabConn(t, c);
      t.database = dbName;
    }
    t.query = q.sql;
    t.title = q.name;
    t.savedId = q.id;
    ui.page = 'query';
    await tick();
    tabStrip?.querySelector('[aria-current=page]')?.scrollIntoView({ inline: 'nearest' });
    if (andRun) void run({ sql: t.query }, t);
    else editor?.commands().focus();
  }

  function deleteQuery(q: SavedQuery) {
    notify(true, `Deleted “${q.name}”`, { label: 'Undo', run: deleteSaved(q.id) });
  }

  /** Generated SQL opens in a new tab on the table's connection and database; nothing runs. */
  function openInNewTab(c: Conn, t: TableRef, sql: string) {
    openSqlTab(c, t.database, sql);
    ui.page = 'query';
  }

  /** Chat SQL opens on the database its step ran on; older steps without one use the active tab's database. */
  function chatTab(sql: string, src?: ChatSource) {
    const c = src && src.connId ? db.connections.find((x) => x.id === src.connId) : undefined;
    return c ? openSqlTab(c, src!.database ?? null, sql) : newTab(sql);
  }

  /** A Run from a chat code block: a new tab there, or the active editor tab the user picked when it is on the same database. */
  async function chatRun(sql: string, src?: ChatSource, target: 'active' | 'new' = 'new') {
    // Writes are inserted for review, never executed straight from an assistant reply.
    const c = src ? db.connections.find((x) => x.id === src.connId) : undefined;
    const engine = c?.engine ?? conn.engine;
    const cls = await classify(engine, sql, src ? dbOf(src, c) : database);
    if (cls.write) {
      ui.page = 'query';
      chatInsert(sql, src);
      return;
    }
    const t = tab;
    const same = !src || !c || sameSource({ connId: t.connId, database: t.database }, { connId: c.id, database: src.database });
    if (target === 'new' || !same) {
      ui.page = 'query';
      void run({ sql }, chatTab(sql, src), cls);
      return;
    }
    if (t.table || runs[t.id] || cls.empty || !confirmDiscard(t.edits)) return;
    ui.page = 'query';
    // Keep the query already in the tab; the new one goes below it as its own statement.
    t.query = engine === 'mongo' ? appendMongoStatement(t.query, sql) : appendStatement(t.query, sql, engine);
    void run({ sql }, t, cls);
  }

  /** Appended to the active editor when it is on the step's database, else into a new tab there. */
  function chatInsert(sql: string, src?: ChatSource) {
    const c = src ? db.connections.find((x) => x.id === src.connId) : undefined;
    const same = !src || !c || sameSource({ connId: tab.connId, database: tab.database }, { connId: c.id, database: src.database });
    if (!tab.table && same) tab.query = conn.engine === 'mongo' ? appendMongoStatement(tab.query, sql) : appendStatement(tab.query, sql, conn.engine);
    else chatTab(sql, src);
  }

  async function tableAction(a: TableAction, c: Conn, t: TableRef) {
    try {
      switch (a) {
        case 'view':
          return openTableView(c, t);
        case 'ddl':
          return openInNewTab(c, t, await api.tableDdl({ connId: c.id, database: t.database, schema: t.schema, table: t.name }));
        case 'select':
        case 'insert': {
          const cols = await api.listColumns({ connId: c.id, database: t.database, schema: t.schema, table: t.name });
          return openInNewTab(c, t, a === 'select' ? selectSql(c.engine, t, cols) : insertSql(c.engine, t, cols));
        }
        case 'count':
          return countRows(c, t);
        case 'exportDdl':
        case 'exportSql':
        case 'exportCsv':
          return exportTable(c, t, a);
        case 'exportDocuments':
          void exportCollection(c, t);
          return;
        case 'truncate':
        case 'drop':
          void askDanger({ action: a, c, t });
      }
    } catch (err) {
      notify(false, ApiError.from(err).message);
    }
  }
</script>

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
                {#if runs[t.id] && !on}<LoaderCircle size={12} class="animate-spin text-faint" />{/if}
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
      <button class="flex h-7 w-full items-center gap-2 rounded-md bg-hover px-2.5 text-muted hover:bg-active hover:text-ink" onclick={() => (overlays.palette = true)}>
        <Search size={13} /> <span class="flex-1 text-left">Quick search</span>
        <kbd class="text-[11px]">{KEYS.palette.hint}</kbd>
      </button>
    </div>

    <div class="seg mb-1.5 flex w-full" role="tablist" aria-label="Sidebar">
      <button class="seg-item flex-1 px-1.5 {ui.sidebarView === 'connections' ? 'seg-on' : ''}" role="tab" aria-selected={ui.sidebarView === 'connections'} onclick={() => (ui.sidebarView = 'connections')}>Connections</button>
      <button class="seg-item flex flex-1 items-center justify-center gap-1.5 px-1.5 {ui.sidebarView === 'queries' ? 'seg-on' : ''}" role="tab" aria-selected={ui.sidebarView === 'queries'} onclick={() => (ui.sidebarView = 'queries')}>Queries {#if saved.list.length}<span class="text-[11px] font-normal text-faint tabular-nums">{saved.list.length}</span>{/if}</button>
    </div>

    <!-- Both stay mounted: the tree keeps its expanded nodes, the list its filter. -->
    <nav class="-mx-1 flex-1 overflow-auto px-1 pb-2" hidden={ui.sidebarView !== 'connections'}>
      <ConnectionTree {selected} onopen={openTableView} onnew={newConnection} onedit={editConnection} onduplicate={duplicateConnection} ondelete={askDeleteConnection} ontable={tableAction} onreorder={reorderConnections} />
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
      <SettingsPage onEnableWrites={enableWrites} />
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
      {@const key = tableLabel(t)}
      {@const columns = tab.result?.columns.length ? tab.result.columns : (schema[key] ?? [])}
      <div class="flex min-h-0 flex-1 flex-col border-t border-line">
        <TableFilterBar {columns} types={tab.result?.types ?? []} bind:filters={tab.filters} loading={!!busy} onapply={() => loadTableData(tab)} />
        <div class="relative min-h-0 flex-1">
          <ResultsGrid tab={tab} loading={!!busy} sort={tab.sort ?? null} onsort={(s) => sortResult(tab, s)} onloadmore={() => loadMore(tab)} onsaved={() => { structureChanged(tab.resultConn); refreshResult(tab); }} foreignKeys={tableFks} onfollow={(fk, row) => followForeignKey(conn, t, fk, row)} />
        </div>
      </div>
    {:else}
      <div class="flex min-h-0 flex-1 border-t border-line">
        {#key tab.id}<TableProperties {conn} table={t} onopen={(r) => openTableView(conn, r)} />{/key}
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
        <span class="shrink-0" title="Commit the tab's open transaction"><button class="btn btn-ghost px-2 @[16rem]:px-2.5" onclick={() => endActive(true)} disabled={!st?.pending || st.aborted || !!busy || txButtons.busy} title="Commit the tab's open transaction" aria-label="Commit"><Check size={14} /> <span class="hidden @[16rem]:inline">Commit</span></button></span>
        <span class="shrink-0" title="Roll back the tab's open transaction"><button class="btn btn-ghost px-2 @[16rem]:px-2.5" onclick={() => endActive(false)} disabled={!(st?.pending || st?.aborted) || !!busy || txButtons.busy} title="Roll back the tab's open transaction" aria-label="Rollback"><Undo2 size={14} /> <span class="hidden @[16rem]:inline">Rollback</span></button></span>
        {#if st?.pending}
          <span class="flex min-w-0 items-center gap-1.5 text-[12px] font-medium text-amber-700 dark:text-amber-400" role="status" title="Transaction open"><span class="size-1.5 shrink-0 rounded-full bg-amber-500"></span> <span class="truncate">Transaction open</span></span>
        {/if}
        {#if txButtons.error?.tab === tab.id}<span class="min-w-0 truncate text-[12px] text-red-700 dark:text-red-400" role="alert">{txButtons.error.text}</span>{/if}
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
      <button class="btn btn-ghost shrink-0 px-2 xl:px-2.5" onclick={() => saveQuery()} disabled={!savedQ && kind === 'empty'} title={savedQ ? (tabClean ? `“${savedQ.name}” is saved · ⇧⌘S saves a copy` : `Save changes to “${savedQ.name}” (${KEYS.save.hint})`) : `Save query (${KEYS.save.hint})`} aria-label={tabClean ? 'Saved' : 'Save query'}>
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
        <button class="btn btn-primary pr-2" onclick={() => run({ sql: currentSql() })} disabled={kind === 'empty'} title="Run the selection, or the statement at the cursor"><Play size={11} class="fill-current" /> Run <kbd class="ml-0.5 hidden font-sans text-[11px] font-normal lg:inline" aria-hidden="true">{KEYS.run.hint}</kbd></button>
      {/if}
    </div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="min-h-28 overflow-hidden {ui.resultsOpen ? 'shrink-0' : 'flex-1'}" style:height={ui.resultsOpen ? `${ui.editorSplit * 100}%` : undefined} oncontextmenu={openEditorMenu}>
      <!-- Keyed so undo never crosses into another tab's text. -->
      {#key tab.id}
        <SqlEditor bind:this={editor} bind:value={tab.query} onrun={() => run({ sql: currentSql() })} onrunnew={() => run({ sql: currentSql(), append: true })} onrunall={() => run({ sql: tab.query })} onformat={formatQuery} {schema} {columnsOf} dialect={conn.engine} />
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
            <ResultsGrid tab={tab} loading={!!busy} sort={tab.result?.sort ?? null} onsort={(s) => sortResult(tab, s)} onloadmore={() => loadMore(tab)} onsaved={() => { structureChanged(tab.resultConn); refreshResult(tab); }} />
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
    <ChatPanel {tables} activeBusy={!!busy} onEnableWrites={enableWrites} onInsertSql={chatInsert} onRunSql={chatRun} />
  {/if}
  </div>
</div>
<svelte:window onclick={() => (editorMenu = null)} oncontextmenu={(e) => { editorMenu = null; blockNativeMenu(e); }} onkeydown={(e) => { if (e.key === 'Escape') editorMenu = null; }} />
{#if editorMenu}
  {@const m = editorMenu}
  <div class="popover fixed z-40 w-64 p-1" style="left:{m.x}px;top:{m.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    <button class="nav-item" role="menuitem" disabled={!!busy} onclick={() => menuAct(() => run({ sql: m.sel ? editor!.commands().selection() : editor!.commands().statementAt() }))}><Play size={13} /> <span class="flex-1">{m.sel ? 'Run selection' : 'Run statement at cursor'}</span><kbd class="kbd">{KEYS.run.hint}</kbd></button>
    <button class="nav-item" role="menuitem" disabled={!!busy || kind === 'empty'} onclick={() => menuAct(() => run({ sql: tab.query }))}><ListOrdered size={13} /> <span class="flex-1">Run all statements</span><kbd class="kbd">{KEYS.runAll.hint}</kbd></button>
    <button class="nav-item" role="menuitem" disabled={!!busy} onclick={() => menuAct(() => scriptInput?.click())}><FileUp size={13} /> Run SQL script from file…</button>
    <button class="nav-item" role="menuitem" disabled={kind === 'empty'} onclick={() => menuAct(formatQuery)}><WandSparkles size={13} /> <span class="flex-1">{m.sel ? 'Format selection' : 'Format query'}</span><kbd class="kbd">{KEYS.format.hint}</kbd></button>
    <div class="mx-2 my-1 h-px bg-line"></div>
    <button class="nav-item" role="menuitem" disabled={!savedQ && kind === 'empty'} onclick={() => menuAct(() => saveQuery())}><Save size={13} /> <span class="flex-1">{savedQ ? 'Save changes' : 'Save query…'}</span><kbd class="kbd">{KEYS.save.hint}</kbd></button>
    <button class="nav-item" role="menuitem" disabled={kind === 'empty'} onclick={() => menuAct(() => saveQuery(true))}><CopyPlus size={13} /> <span class="flex-1">Save as new query…</span><kbd class="kbd">{KEYS.saveAs.hint}</kbd></button>
    <div class="mx-2 my-1 h-px bg-line"></div>
    <button class="nav-item" role="menuitem" disabled={!m.sel} onclick={() => menuAct(() => copySelection(true))}><Scissors size={13} /> Cut</button>
    <button class="nav-item" role="menuitem" disabled={!m.sel} onclick={() => menuAct(() => copySelection(false))}><Copy size={13} /> Copy</button>
    <button class="nav-item" role="menuitem" onclick={() => menuAct(() => navigator.clipboard?.readText().then((s) => editor?.commands().insert(s), () => {}))}><ClipboardPaste size={13} /> Paste</button>
    <button class="nav-item" role="menuitem" onclick={() => menuAct(() => editor?.commands().selectAll())}><span class="size-[13px]"></span> Select all</button>
  </div>
{/if}
<ConnectionDialog bind:open={overlays.connection} conn={connEdit} canswitch={canSwitch} canupdate={canUpdate} onsaved={openFirstTable} onedited={connEdited} />
<WhatsNewDialog />
<NoticeToast />
<CommandPalette
  bind:open={overlays.palette}
  tables={paletteTables}
  onfocuseditor={focusEditor}
  onrun={() => { if (kind === 'empty') return; ui.page = 'query'; run({ sql: currentSql() }); }}
  ontogglechat={() => (ui.chatOpen = !ui.chatOpen)}
  ontogglesidebar={() => (ui.sidebarOpen = !ui.sidebarOpen)}
  onpage={(p) => (ui.page = p)}
  ontoggletheme={toggleTheme}
  onnewconnection={newConnection}
  onswitch={(c) => { ui.page = 'query'; switchConn(c); }}
  onsave={() => saveQuery()}
  onopensaved={(q) => openSaved(q)}
/>
<!-- Last, so an answer stacks above the connection dialog and any dialog that waits on it. -->
<AskHost />
