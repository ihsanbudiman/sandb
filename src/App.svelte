<script lang="ts">
  import EnvironmentBadge from './lib/components/EnvironmentBadge.svelte';
  import { Database, Search, Settings, ScrollText, Sun, Moon, Plus, Sparkles, PanelRight, PanelLeft, CircleArrowUp, PanelBottom, CalendarClock } from '@lucide/svelte';
  import { FileCode, Play, Square, X, LoaderCircle, Check, Undo2, ListOrdered, FileUp, Scissors, Copy, ClipboardPaste, ChevronUp, ChevronDown, Table2, Eye, ScanEye, RotateCw, Save, CopyPlus, WandSparkles, Pencil } from '@lucide/svelte';
  import { tick, untrack } from 'svelte';
  import { fade } from 'svelte/transition';
  import { canWrite, db, duplicateDraft, isRedis, isSql, sameSource, sameTarget, setAiWrite, dbOf, defaultDb, type Conn } from './lib/connections.svelte';
  import { ui } from './lib/state.svelte';
  import { look, backdropOn, toggleTheme } from './lib/appearance.svelte';
  import { catalog, completionColumns, completionLinks, forget as forgetCatalog, loadSnapshot, loadedTables, properties, snapshot, structureChanged } from './lib/catalog.svelte';
  import { openChatOnTab } from './lib/chat.svelte';
  import type { Source as ChatSource } from './lib/connections.svelte';
  import { api, ApiError, type QueryResult } from './lib/api';
  import { appendStatement } from './lib/sql/text';
  import { appendMongoStatement } from './lib/mongo/text';
  import { appendRedisStatement } from './lib/redis/text';
  import { snapshotSchema } from './lib/results';
  import { confirmDiscard } from './lib/edits';
  import { tabs, activeTab, active, newTab, selectTab, closeTab, setTabConn, openTableTab, openSqlTab, clearResults, activateResult, closeResult, pendingOf, tabDatabase, isPlaceholderTitle, type Tab } from './lib/tabs.svelte';
  import { saved, findSaved, isDirty, suggestName, createSaved, updateSaved, deleteSaved, renameSaved, type SavedQuery } from './lib/saved.svelte';
  import { displayName, selectSql, insertSql, type TableAction, type TableFilter, type TableRef } from './lib/tableActions';
  import { tableKey, tableIdOf, tableLabel } from './lib/tableId';
  import { runs, clock, run, cancel, cancelRun, loadTableData, loadMore, sortResult, refreshResult, countRows, exportTable, exportCollection, askDanger } from './lib/run.svelte';
  import { manualCommit, refreshStatus, leaveSession, settlePending, txButtons, endActive } from './lib/tx.svelte';
  import { schedules, unseen, unseenFailed, newSchedule, scheduleFromSaved } from './lib/schedules.svelte';
  import { ask, confirm } from './lib/ask.svelte';
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
  import SchedulerPage from './lib/components/SchedulerPage.svelte';
  import RedisKeyDialog from './lib/components/RedisKeyDialog.svelte';
  import SaveQueryDialog from './lib/components/SaveQueryDialog.svelte';
  import NoticeToast from './lib/components/NoticeToast.svelte';
  import WhatsNewDialog from './lib/components/WhatsNewDialog.svelte';
  import AskHost from './lib/components/AskHost.svelte';
  import GridToolbar from './lib/components/GridToolbar.svelte';
  import Menu from './lib/components/ui/Menu.svelte';
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
  let panesEl = $state<HTMLDivElement>();
  let viewportWidth = $state(window.innerWidth);
  const dockChat = $derived(viewportWidth >= 1100);
  // Keep room for the workspace; when the window is smaller than that budget, keep the overlay usable.
  const chatWidthLimit = $derived(Math.min(640, Math.max(Math.min(240, viewportWidth - 16), viewportWidth - 560)));
  const chatWidth = $derived(Math.min(ui.chatWidth, chatWidthLimit));
  const sidebarWidthLimit = $derived(clamp(viewportWidth - (dockChat && ui.chatOpen ? chatWidth : 0) - 324, 140, 480));
  const sidebarWidth = $derived(Math.min(ui.sidebarWidth, sidebarWidthLimit));
  // The overlay chat covers the editor and its Run button. Entering the narrow layout (or starting in it)
  // closes the chat once; the user can open it again with the toggle.
  let wasDocked = true;
  $effect.pre(() => {
    if (wasDocked && !dockChat) ui.chatOpen = false;
    wasDocked = dockChat;
  });
  let chatHidden = $state(!ui.chatOpen);
  let resizingChat = $state(false);
  let chatToggle = $state<HTMLButtonElement>();
  let chatPanel = $state<HTMLDivElement>();

  $effect.pre(() => {
    if (ui.chatOpen) {
      chatHidden = false;
    } else {
      if (chatPanel?.contains(document.activeElement)) chatToggle?.focus();
      if (matchMedia('(prefers-reduced-motion: reduce)').matches) chatHidden = true;
      else {
        const timer = setTimeout(() => (chatHidden = true), 160);
        return () => clearTimeout(timer);
      }
    }
  });

  // Pointer capture keeps the moves coming after the pointer leaves the thin handle.
  function drag(e: PointerEvent, move: (dx: number, dy: number) => void, end?: () => void) {
    if (e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    const [x, y] = [e.clientX, e.clientY];
    el.setPointerCapture(e.pointerId);
    el.onpointermove = (m) => move(m.clientX - x, m.clientY - y);
    el.onlostpointercapture = () => {
      el.onpointermove = el.onlostpointercapture = null;
      end?.();
    };
  }

  const setSidebar = (w: number) => (ui.sidebarWidth = Math.round(clamp(w, 140, sidebarWidthLimit)));
  const setChat = (w: number) => (ui.chatWidth = Math.round(clamp(w, 240, chatWidthLimit)));
  const setSplit = (px: number) => panesEl?.clientHeight && (ui.editorSplit = clamp(px / panesEl.clientHeight, 0.1, 0.8));

  function resizeSidebar(e: PointerEvent) {
    const w = sidebarWidth;
    drag(e, (dx) => setSidebar(w + dx));
  }

  // The chat panel is on the right: a drag to the left makes it wider.
  function resizeChat(e: PointerEvent) {
    if (e.button !== 0) return;
    const w = chatWidth;
    resizingChat = true;
    drag(e, (dx) => setChat(w - dx), () => (resizingChat = false));
  }

  function resizeEditor(e: PointerEvent) {
    const h = ui.editorSplit * (panesEl?.clientHeight ?? 0);
    restoreSplit = null;
    drag(e, (_, dy) => setSplit(h + dy));
  }

  // Arrow keys move a focused splitter by 16 px.
  function keySidebar(e: KeyboardEvent) {
    const d = { ArrowLeft: -16, ArrowRight: 16 }[e.key];
    if (d) { e.preventDefault(); setSidebar(sidebarWidth + d); }
  }

  function keyChat(e: KeyboardEvent) {
    const d = { ArrowLeft: 16, ArrowRight: -16 }[e.key];
    if (d) { e.preventDefault(); setChat(chatWidth + d); }
  }

  function keyEditor(e: KeyboardEvent) {
    const d = { ArrowUp: -16, ArrowDown: 16 }[e.key];
    if (d && panesEl) { e.preventDefault(); restoreSplit = null; setSplit(ui.editorSplit * panesEl.clientHeight + d); }
  }

  function sidebarKey(e: KeyboardEvent) {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
    e.preventDefault();
    ui.sidebarView = e.key === 'Home' ? 'connections' : e.key === 'End' ? 'queries' : ui.sidebarView === 'connections' ? 'queries' : 'connections';
    document.getElementById(`sidebar-tab-${ui.sidebarView}`)?.focus();
  }

  async function toggleResults() {
    ui.resultsOpen = !ui.resultsOpen;
    await tick();
    mainEl?.querySelector<HTMLButtonElement>('[data-results-toggle]:not([hidden] *)')?.focus();
  }

  // The toolbar arrows move the sash to a limit; the opposite arrow restores the normal split.
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

  let switching = $state<'conn' | 'db' | null>(null);

  async function switchTarget(target: { conn: Conn } | { database: string }) {
    if (switching) return;
    const t = tab;
    const c = conn;
    const previousDb = t.database;
    const result = t.result;
    const connSwitch = 'conn' in target;
    if (connSwitch ? target.conn.id === c.id : c.engine === 'sqlite' || target.database === database) return;
    switching = connSwitch ? 'conn' : 'db';
    const stillValid = () => t === tab && conn === c && t.database === previousDb && t.result === result && (!connSwitch || db.connections.includes(target.conn));
    try {
      if (!await confirmDiscard(t.edits) || !stillValid()) return;
      if (!await settlePending([t], connSwitch ? 'Switching the connection' : 'Switching the database') || !stillValid()) return;
      leaveSession(t);
      if (connSwitch) setTabConn(t, target.conn);
      else t.database = target.database;
      openFirstTable();
    } finally {
      switching = null;
    }
  }

  // Double-click, ⌘-click or "View data" in the tree opens a table or key tab.
  // A tab that already shows the table comes to the front instead of a second one.
  // `filters`: replace the tab's filters and load again, also when the tab already exists (foreign key links).
  async function openTableView(c: Conn, t: TableRef, { filters, view }: { filters?: TableFilter[]; view?: Tab['view'] } = {}) {
    const redis = isRedis(c);
    // A table opens in its own tab (or picks the open one): the current tab keeps its edits.
    const { tab: nt, created } = openTableTab(c, t);
    if (view === 'properties') nt.view = 'properties';
    else if (redis) nt.view = 'data';
    else if (filters) {
      nt.filters = filters;
      nt.view = 'data';
    }
    ui.page = 'query';
    await tick();
    tabStrip?.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    if (nt.view === 'data' && (created || filters || (redis && !nt.result))) await loadTableData(nt);
  }

  /** Opens the referenced table, filtered to the row that `row` points at. */
  function followForeignKey(c: Conn, from: TableRef, fk: { columns: string[]; refSchema?: string | null; refTable: string; refColumns: string[] }, row: Record<string, unknown>) {
    const values = fk.columns.map((col) => row[col]);
    if (values.some((v) => v === null || typeof v === 'object')) return;
    const ref: TableRef = { database: from.database, schema: fk.refSchema ?? null, name: fk.refTable, kind: 'table' };
    openTableView(c, ref, { filters: fk.refColumns.map((column, i) => ({ id: crypto.randomUUID(), column, operator: '=', value: String(values[i]) })) });
  }

  // The tabs stay in the title bar on every page; picking one goes back to the query view.
  function pickTab(id: string) {
    if (id !== tab.id) selectTab(id);
    ui.page = 'query';
  }

  let tabStrip = $state<HTMLDivElement>();
  let tabRow = $state<HTMLDivElement>();
  let tabMenu = $state<{ t: Tab; trigger: HTMLButtonElement; anchor: HTMLButtonElement | { x: number; y: number } } | null>(null);
  let fadeLeft = $state(false);
  let fadeRight = $state(false);

  function updateTabEdges() {
    if (!tabStrip) return;
    fadeLeft = tabStrip.scrollLeft > 1;
    fadeRight = tabStrip.scrollLeft + tabStrip.clientWidth < tabStrip.scrollWidth - 1;
  }

  $effect(() => {
    if (!tabStrip || !tabRow) return;
    const observer = new ResizeObserver(updateTabEdges);
    observer.observe(tabStrip);
    observer.observe(tabRow);
    return () => observer.disconnect();
  });

  function tabKey(e: KeyboardEvent, t: Tab) {
    if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
      e.preventDefault();
      editorMenu = null;
      const trigger = e.currentTarget as HTMLButtonElement;
      tabMenu = { t, trigger, anchor: trigger };
      return;
    }
    const list = tabs.list;
    const at = list.indexOf(t);
    const next = e.key === 'ArrowLeft' ? (at + list.length - 1) % list.length : e.key === 'ArrowRight' ? (at + 1) % list.length : e.key === 'Home' ? 0 : e.key === 'End' ? list.length - 1 : -1;
    if (next < 0) return;
    e.preventDefault();
    pickTab(list[next].id);
    document.getElementById(`query-tab-${list[next].id}`)?.focus();
  }

  function openTabMenu(e: MouseEvent, t: Tab) {
    e.preventDefault();
    e.stopPropagation();
    editorMenu = null;
    const trigger = e.currentTarget as HTMLButtonElement;
    tabMenu = { t, trigger, anchor: e.clientX || e.clientY ? { x: e.clientX, y: e.clientY } : trigger };
  }
  async function addTab() {
    // A new tab keeps the edits of the tab it leaves: it never asks to discard them.
    newTab();
    ui.page = 'query';
    await tick();
    tabStrip?.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }

  // Each close gets the same safeguards, including when invoked from Close others.
  async function dropTab(t: Tab) {
    if (t.edits.saving) return false;
    if (isDirty(t) && !await confirm({ title: 'Close tab without saving?', body: `“${t.title}” has unsaved changes.`, confirmLabel: 'Close tab', destructive: true })) return false;
    if (t.id !== tab.id && pendingOf(t) && !await confirm({ title: 'Close tab without saving?', body: `“${t.title}” has unsaved grid changes.`, confirmLabel: 'Close tab', destructive: true })) return false;
    if (t.id === tab.id && !await confirmDiscard(t.edits)) return false;
    if (!await settlePending([t], 'Closing this tab') || !tabs.list.includes(t) || t.edits.saving) return false;
    const hadFocus = document.getElementById(`query-tab-${t.id}`)?.parentElement?.contains(document.activeElement);
    cancelRun(t);
    closeTab(t.id);
    leaveSession(t);
    if (hadFocus) {
      await tick();
      document.getElementById(`query-tab-${tab.id}`)?.focus();
    }
    return true;
  }

  async function closeOtherTabs(keep: Tab) {
    for (const t of tabs.list.filter((t) => t !== keep)) {
      if (!await dropTab(t)) break;
    }
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

  async function keyRename(e: KeyboardEvent, t: Tab) {
    if (e.key !== 'Enter' && e.key !== 'Escape') return;
    e.preventDefault();
    if (e.key === 'Escape') renaming = null;
    else rename(t, (e.currentTarget as HTMLInputElement).value);
    await tick();
    document.getElementById(`query-tab-${t.id}`)?.focus();
  }

  function focusSelect(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

  const conn = $derived(active());
  const hasConn = $derived(db.connections.length > 0);
  const database = $derived(tabDatabase(tab, conn));
  // Autocomplete and palette tables of the active database; the catalog loads them once per database.
  const snap = $derived(snapshot(conn.id, database));
  // Autocomplete tables: the snapshot, kept while it reloads. When it has no tables (still loading, failed,
  // or not loaded), the table lists that the tree loaded for this database stand in, without columns.
  const snapItems = $derived.by(() => {
    const s = snap && snap.status !== 'error' ? snap.items : null;
    if (s?.tables.length) return s;
    const listed = loadedTables()
      .filter(({ c, t, partition }) => c.id === conn.id && t.database === database && !partition)
      .map(({ t }) => ({ ...(t.schema ? { schema: t.schema } : {}), name: t.name, kind: t.kind, columns: [] }));
    return listed.length ? { database, tables: listed, truncated: true } : s;
  });
  const tablesState = $derived(!snap || snap.status === 'loading' ? { status: 'loading' as const } : snap.status === 'error' ? { status: 'error' as const, error: snap.error } : { status: 'ok' as const });
  const schema = $derived(snapshotSchema(snapItems));
  const tables = $derived(Object.keys(schema));
  // Tree highlight follows the shown result; joins, unions, CTEs and aggregates have no single table.
  const selected = $derived.by(() => {
    const { result: r, opened } = tab;
    return opened && r && !r.error && r.sql === opened.sql ? tableKey(opened.id) : null;
  });
  // Only SQL statements decide the Read/Write cues. Other editors only need to know if there is text to run.
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
    const [id, dbName] = [conn.id, database];
    // Untracked: the load reads the snapshot state, so a tracked call re-ran this effect on every state
    // change, and a failed snapshot was asked again in an endless loop that never settled.
    void untrack(() => loadSnapshot(id, dbName)).then(() => {
      const s = snapshot(id, dbName);
      // A failed snapshot leaves autocomplete without tables; say why instead of an empty list.
      if (s?.status === 'error') notify(false, `Autocomplete could not load the ${isRedis(conn) ? 'keys' : 'tables'} of ${dbName}: ${s.error}`);
    });
  });

  /** Autocomplete columns beyond the snapshot and join foreign keys; the catalog answers from its own caches. */
  const columnsOf = (path: string[]) => completionColumns(conn.id, database, path);
  const linksOf = (path: string[]) => completionLinks(conn.id, database, path);

  /** The SQL a plain Run executes: the selection, or the statement at the cursor, or the whole query. */
  function currentSql(): string {
    if (!editor) return tab.query;
    const { selection, statementAt } = editor.commands();
    const selectedText = selection();
    const trimmed = selectedText.trim();
    return trimmed ? (isRedis(conn) ? selectedText : trimmed) : statementAt();
  }

  // Editor context menu. It replaces the native menu, so it carries Cut / Copy / Paste too.
  let editorMenu = $state<{ x: number; y: number; sel: boolean } | null>(null);

  function openEditorMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    tabMenu = null;
    editor?.commands().pointAt(e.clientX, e.clientY);
    editorMenu = { x: e.clientX, y: e.clientY, sel: !!editor?.commands().selection() };
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
    const t = tab;
    const result = t.result;
    if (!await confirmDiscard(t.edits) || t !== tab || t.result !== result) return false;
    if (!await settlePending([t], 'Switching the connection') || t !== tab || t.result !== result) return false;
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
    return confirm({ title: 'Discard unsaved grid changes?', body: `This connection change discards ${n} unsaved grid change${n === 1 ? '' : 's'} in ${counts.length} tab${counts.length === 1 ? '' : 's'}.`, confirmLabel: 'Discard changes', destructive: true });
  }

  // Tauri: closing the window or menu Quit with a pending transaction asks here first; the backend rolls back
  // whatever is left. A quit during another tab prompt waits for it (Cancel there cancels the quit too).
  $effect(() => {
    if (!api.capabilities.quitPrompt) return;
    let off: (() => void) | undefined;
    let gone = false;
    let quitting = false;
    import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen('sandb://quit-requested', async () => {
          if (quitting || tabs.list.some((t) => t.edits.saving)) return;
          quitting = true;
          try {
            if ((tabs.list.some((t) => pendingOf(t)) || tabs.list.some((t) => t.status?.pending)) && !await confirm({ title: 'Quit without saving?', body: 'There are unsaved grid changes. Quit without saving them?', confirmLabel: 'Quit', destructive: true })) return;
            if (await settlePending(tabs.list, 'Quitting sandb', true)) await api.quitApp().catch(() => {});
          } finally {
            quitting = false;
          }
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

  /** Switching result tabs drops edit mode but keeps the grid's per-result presentation state. */
  async function openResult(t: Tab, r: QueryResult) {
    const previous = t.result;
    if (previous === r || !await confirmDiscard(t.edits) || t !== tab || t.result !== previous) return;
    const i = t.results.indexOf(r);
    if (i >= 0) activateResult(t, i);
  }

  /** The X of one result tab; closing the last one hides the results pane. */
  async function dropResult(t: Tab, r: QueryResult) {
    const previous = t.result;
    if (previous === r && !await confirmDiscard(t.edits)) return;
    const i = t.results.indexOf(r);
    if (t !== tab || i < 0 || t.result !== previous) return;
    closeResult(t, i);
    if (!t.results.length) {
      ui.resultsOpen = false;
      await tick();
      mainEl?.querySelector<HTMLButtonElement>('[data-results-toggle]')?.focus();
    }
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
    const t = tab;
    const result = t.result;
    if (wasActive && (!await confirmDiscard(t.edits) || t !== tab || t.result !== result)) return false;
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

  // Palette tables: the active database's snapshot plus lists the tree has loaded, including Redis key searches.
  const paletteTables = $derived.by<PaletteTable[]>(() => {
    const here = `${conn.name} / ${database}`;
    const out: PaletteTable[] = [];
    const seen = new Set<string>();
    for (const t of !isRedis(conn) && snap?.status === 'ok' ? (snap.items?.tables ?? []) : []) {
      const label = tableLabel(t);
      if (seen.has(label)) continue;
      seen.add(label);
      out.push({ key: `${conn.id}/${database}/${label}`, label, hint: here, open: () => openTableView(conn, { database, schema: t.schema ?? null, name: t.name, kind: t.kind }) });
    }
    for (const { c, t } of loadedTables()) {
      const redis = isRedis(c);
      if (!redis && c.id === conn.id && t.database === database) continue;
      const open = () => { ui.page = 'query'; openTableView(c, t); };
      out.push({ key: tableKey(tableIdOf(c.id, t)), label: tableLabel(t), hint: `${c.name} / ${redis ? `db${t.database}` : t.database}`, open });
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
    tabStrip?.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
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
    if (t.table || runs[t.id] || cls.empty) return;
    const targetResult = t.result;
    const targetDatabase = t.database;
    if (!await confirmDiscard(t.edits) || t !== tab || t.result !== targetResult || t.database !== targetDatabase || runs[t.id]) return;
    ui.page = 'query';
    // Keep the query already in the tab; the new one goes below it as its own statement.
    if (engine === 'mongo') t.query = appendMongoStatement(t.query, sql);
    else if (engine === 'redis') t.query = appendRedisStatement(t.query, sql);
    else t.query = appendStatement(t.query, sql, engine);
    void run({ sql }, t, cls);
  }

  /** Appended to the active editor when it is on the step's database, else into a new tab there. */
  function chatInsert(sql: string, src?: ChatSource) {
    const c = src ? db.connections.find((x) => x.id === src.connId) : undefined;
    const same = !src || !c || sameSource({ connId: tab.connId, database: tab.database }, { connId: c.id, database: src.database });
    if (tab.table || !same) chatTab(sql, src);
    else if (conn.engine === 'mongo') tab.query = appendMongoStatement(tab.query, sql);
    else if (conn.engine === 'redis') tab.query = appendRedisStatement(tab.query, sql);
    else tab.query = appendStatement(tab.query, sql, conn.engine);
  }

  const tableBusy = $state<Record<string, boolean>>({});

  async function runRedisKeyCommand(original: Conn, database: string, sql: string | null) {
    if (!sql) return;
    // An edit or deletion during confirmation must not send the command to a different target.
    const c = db.connections.find((c) => c.id === original.id);
    if (!c || !isRedis(c) || !sameTarget(c, original) || c.ssl !== original.ssl || !canWrite(c)) {
      notify(false, 'The Redis connection changed. Open the key action again.');
      return;
    }
    ui.page = 'query';
    await run({ sql }, openSqlTab(c, database, sql));
  }

  async function newRedisKey(c: Conn, database: string) {
    const original = $state.snapshot(c);
    const sql = await ask<string | null>(RedisKeyDialog, { conn: c, target: { mode: 'new', database } });
    await runRedisKeyCommand(original, database, sql);
  }

  async function tableAction(a: TableAction, c: Conn, t: TableRef) {
    const key = tableKey(tableIdOf(c.id, t));
    if (tableBusy[key]) return;
    tableBusy[key] = true;
    try {
      switch (a) {
        case 'view':
          return await openTableView(c, t);
        case 'properties':
          return await openTableView(c, t, { view: 'properties' });
        case 'renameKey':
        case 'ttlKey': {
          const original = $state.snapshot(c);
          const mode = a === 'renameKey' ? 'rename' : 'ttl';
          const sql = await ask<string | null>(RedisKeyDialog, { conn: c, target: { mode, database: t.database, key: t.name } });
          return await runRedisKeyCommand(original, t.database, sql);
        }
        case 'deleteKey': {
          const original = $state.snapshot(c);
          const sql = `DEL ${t.name}`;
          if (await confirm({ title: 'Delete Redis key?', body: `${c.name} / db${t.database}\n${sql}`, confirmLabel: 'Delete key', destructive: true })) {
            await runRedisKeyCommand(original, t.database, sql);
          }
          return;
        }
        case 'ddl':
          return openInNewTab(c, t, await api.tableDdl({ connId: c.id, database: t.database, schema: t.schema, table: t.name }));
        case 'select':
        case 'insert': {
          const cols = await api.listColumns({ connId: c.id, database: t.database, schema: t.schema, table: t.name });
          return openInNewTab(c, t, a === 'select' ? selectSql(c.engine, t, cols) : insertSql(c.engine, t, cols));
        }
        case 'count':
          return await countRows(c, t);
        case 'exportDdl':
        case 'exportSql':
        case 'exportCsv':
          return await exportTable(c, t, a);
        case 'exportDocuments':
          return await exportCollection(c, t);
        case 'truncate':
        case 'drop':
          await askDanger({ action: a, c, t });
      }
    } catch (err) {
      notify(false, ApiError.from(err).message);
    } finally {
      delete tableBusy[key];
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
    <div class="flex h-full min-w-max shrink-0 items-center gap-2 {inset ? 'pl-[76px]' : 'pl-2.5'}" style:width={ui.sidebarOpen ? `${sidebarWidth}px` : undefined} data-tauri-drag-region>
      <!-- The app icon's spark: a horizontal bar and two bars at ±60°. -->
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.75" stroke-linecap="round" class="shrink-0 text-accent" aria-hidden="true"><path d="M5 12h14M8.5 5.938l7 12.124M15.5 5.938l-7 12.124" /></svg>
      <span class="text-base font-semibold tracking-[-0.01em]" data-tauri-drag-region>sandb</span>
      <button class="btn btn-ghost btn-on ml-auto size-7 shrink-0 p-0" onclick={() => (ui.sidebarOpen = !ui.sidebarOpen)} title={ui.sidebarOpen ? 'Hide sidebar' : 'Show sidebar'} aria-label="Toggle sidebar" aria-pressed={ui.sidebarOpen}><PanelLeft size={15} /></button>
    </div>
    {#if hasConn}
      <!-- The strip starts at the main card's left edge, so the first tab lines up with the card below. -->
      <div class="relative ml-1.5 h-full min-w-0">
        <div bind:this={tabStrip} onscroll={updateTabEdges} class="h-full overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden" role="tablist" aria-label="Workspace tabs">
          <div bind:this={tabRow} role="presentation" class="flex h-full w-max items-center gap-1 px-0.5">
        {#each tabs.list as t (t.id)}
          {@const on = t.id === tab.id}
          {@const dirtyT = isDirty(t)}
          {@const prodT = db.connections.find((c) => c.id === t.connId)?.env === 'production'}
          <!-- Production tabs carry a red underline, so the cue shows before the tab is opened. -->
          <div role="presentation" class="group relative flex h-7 shrink-0 items-center rounded-md text-sm {on ? 'tab-on font-medium text-ink' : 'text-muted hover:bg-hover hover:text-ink'}">
            {#if prodT}<span class="pointer-events-none absolute inset-x-1.5 bottom-0 h-0.5 rounded-full bg-danger" aria-hidden="true"></span>{/if}
            {#if renaming === t.id}
              <span class="flex items-center gap-2 pl-2.5"><FileCode size={13} class="text-faint" />
                <input class="focus-ring w-28 rounded-sm bg-transparent" value={t.title} aria-label="Tab name" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" use:focusSelect onblur={(e) => rename(t, e.currentTarget.value)} onkeydown={(e) => keyRename(e, t)} />
              </span>
            {:else}
              <button class="flex h-full items-center gap-2 rounded-md pl-2.5" id="query-tab-{t.id}" role="tab" aria-selected={on} aria-controls={on ? 'workspace-panel' : undefined} tabindex={on ? 0 : -1} aria-haspopup="menu" onclick={() => pickTab(t.id)} ondblclick={() => (renaming = t.id)} onkeydown={(e) => tabKey(e, t)} oncontextmenu={(e) => openTabMenu(e, t)} title="{prodT ? 'Protected writes · ' : ''}Double-click to rename">
                {#if t.table}<Table2 size={13} class={on ? 'text-accent' : 'text-faint'} />{:else}<FileCode size={13} class={on ? 'text-accent' : 'text-faint'} />{/if} <span class="max-w-40 truncate">{t.title}</span>{#if prodT}<span class="sr-only">, protected writes</span>{/if}
                {#if runs[t.id] && (!on || ui.page !== 'query')}<LoaderCircle size={12} class="text-faint motion-safe:animate-spin" />{/if}
                {#if t.status?.txOpen}<span class="size-1.5 rounded-full bg-warn" title="Transaction open"></span>{/if}
              </button>
            {/if}
            <!-- Unsaved changes to a saved query: a dot, as in code editors; it turns into the X on hover. -->
            <button class="focus-ring mr-1 ml-1.5 grid size-[18px] place-items-center rounded text-faint hover:bg-hover hover:text-ink {on || dirtyT ? '' : 'opacity-0 group-hover:opacity-100 focus-visible:opacity-100'}" tabindex={on ? 0 : -1} title={dirtyT ? 'Unsaved changes · close tab' : 'Close tab'} aria-label="Close {t.title}{dirtyT ? ' (unsaved changes)' : ''}" onclick={() => dropTab(t)}>
              {#if dirtyT}<span class="size-2 rounded-full bg-accent group-hover:hidden"></span><X size={12} class="hidden group-hover:block" />{:else}<X size={12} />{/if}
            </button>
          </div>
        {/each}
          </div>
        </div>
        {#if fadeLeft}<span class="pointer-events-none absolute inset-y-0 left-0 w-3 bg-linear-to-r from-canvas to-transparent" aria-hidden="true"></span>{/if}
        {#if fadeRight}<span class="pointer-events-none absolute inset-y-0 right-0 w-3 bg-linear-to-l from-canvas to-transparent" aria-hidden="true"></span>{/if}
      </div>
      <button class="btn btn-ghost ml-1 size-7 shrink-0 p-0" title="New query" aria-label="New query" onclick={addTab}><Plus size={15} /></button>
    {/if}
    <div class="h-full min-w-6 flex-1" data-tauri-drag-region></div>
    {#if updates.status === 'available' || updates.status === 'restart'}
      <button class="btn btn-ghost mr-1 shrink-0 text-warn" title="sandb v{updates.next} is available" onclick={() => { ui.settingsSection = 'updates'; ui.page = 'settings'; }}><CircleArrowUp size={13} /> Update</button>
    {/if}
    <button bind:this={chatToggle} class="btn btn-ghost btn-on size-7 shrink-0 p-0" aria-pressed={ui.chatOpen} aria-controls="assistant-panel" onclick={() => (ui.chatOpen = !ui.chatOpen)} title="Toggle AI chat" aria-label="Toggle AI chat"><PanelRight size={15} /></button>
  </div>

  <div class="relative flex min-h-0 flex-1 gap-2 overflow-clip px-2 pb-2">
  <!-- Sidebar: sits on the canvas, so it reads as one piece with the title bar.
       Hidden, not unmounted, so the tree keeps its expanded nodes. -->
  <aside class="flex shrink-0 flex-col overflow-hidden" style:width="{sidebarWidth}px" hidden={!ui.sidebarOpen}>
    <div class="pt-1 pb-2">
      <button class="btn btn-ghost w-full gap-2 bg-hover px-2.5 hover:bg-active" onclick={() => (overlays.palette = true)}>
        <Search size={13} /> <span class="flex-1 text-left">Quick search</span>
        <kbd class="text-xs">{KEYS.palette.hint}</kbd>
      </button>
    </div>

    <div class="seg mb-1.5 flex w-full" role="tablist" aria-label="Sidebar">
      <button id="sidebar-tab-connections" class="seg-item flex-1 px-1.5 {ui.sidebarView === 'connections' ? 'seg-on' : ''}" role="tab" aria-controls="sidebar-connections" aria-selected={ui.sidebarView === 'connections'} tabindex={ui.sidebarView === 'connections' ? 0 : -1} onkeydown={sidebarKey} onclick={() => (ui.sidebarView = 'connections')}>Connections</button>
      <button id="sidebar-tab-queries" class="seg-item flex flex-1 items-center justify-center gap-1.5 px-1.5 {ui.sidebarView === 'queries' ? 'seg-on' : ''}" role="tab" aria-controls="sidebar-queries" aria-selected={ui.sidebarView === 'queries'} tabindex={ui.sidebarView === 'queries' ? 0 : -1} onkeydown={sidebarKey} onclick={() => (ui.sidebarView = 'queries')}>Queries {#if saved.list.length}<span class="text-xs font-normal text-faint tabular-nums">{saved.list.length}</span>{/if}</button>
    </div>

    <!-- Both stay mounted: the tree keeps its expanded nodes, the list its filter. -->
    <div id="sidebar-connections" role="tabpanel" aria-labelledby="sidebar-tab-connections" class="min-h-0 flex-1 overflow-auto pb-2 [scrollbar-gutter:stable]" hidden={ui.sidebarView !== 'connections'}>
      <ConnectionTree {selected} {tableBusy} onopen={openTableView} onnew={newConnection} onedit={editConnection} onduplicate={duplicateConnection} ondelete={askDeleteConnection} ontable={tableAction} onredisnew={newRedisKey} onreorder={reorderConnections} />
    </div>
    <div id="sidebar-queries" role="tabpanel" aria-labelledby="sidebar-tab-queries" class="min-h-0 flex-1 overflow-auto pb-2 [scrollbar-gutter:stable]" hidden={ui.sidebarView !== 'queries'}>
      <SavedQueries activeId={ui.page === 'query' ? (savedQ?.id ?? null) : null} onopen={(q) => openSaved(q)} onrun={(q) => openSaved(q, true)} onschedule={(q) => newSchedule(scheduleFromSaved(q))} ondelete={deleteQuery} />
    </div>

    <button class="nav-item shrink-0 {ui.page === 'scheduler' ? 'nav-item-active' : ''}" aria-current={ui.page === 'scheduler' ? 'page' : undefined} onclick={() => (ui.page = ui.page === 'scheduler' ? 'query' : 'scheduler')}>
      <CalendarClock size={13} /> <span class="flex-1">Schedules</span>
      {#if unseenFailed()}
        <span class="size-1.5 shrink-0 rounded-full bg-danger" aria-label="A scheduled run failed"></span>
      {:else if schedules.list.some((s) => unseen(s) > 0)}
        <span class="size-1.5 shrink-0 rounded-full bg-accent" aria-label="New scheduled runs"></span>
      {:else if schedules.list.length}
        <span class="text-xs font-normal text-faint tabular-nums">{schedules.list.length}</span>
      {/if}
    </button>

    <div class="flex items-center gap-0.5 pt-1">
      <button class="nav-item min-w-0 flex-1 {ui.page === 'settings' ? 'nav-item-active' : ''}" onclick={() => (ui.page = ui.page === 'settings' ? 'query' : 'settings')}><Settings size={13} /> Settings</button>
      <button class="nav-item size-list-row justify-center p-0 {ui.page === 'audit' ? 'nav-item-active' : ''}" title="Audit log" aria-label="Audit log" onclick={() => (ui.page = ui.page === 'audit' ? 'query' : 'audit')}><ScrollText size={13} /></button>
      <button class="btn btn-ghost size-list-row p-0" title="Toggle theme" onclick={toggleTheme}>
        {#if ui.dark}<Sun size={13} />{:else}<Moon size={13} />{/if}
      </button>
    </div>
  </aside>

  <!-- Fills the gap between the sidebar and the card: the negative margins cancel its share of the flex gap.
       A focusable separator is the ARIA window-splitter pattern; Svelte counts every separator as static. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="-mx-2 w-2 shrink-0 cursor-col-resize touch-none rounded-full outline-none hover:bg-accent/25 focus-visible:bg-accent/40" role="separator" aria-orientation="vertical" aria-label="Resize sidebar" aria-valuenow={sidebarWidth} aria-valuemin={140} aria-valuemax={sidebarWidthLimit} tabindex="0" hidden={!ui.sidebarOpen} onpointerdown={resizeSidebar} onkeydown={keySidebar}></div>

  <!-- Main -->
  <!-- `overflow-clip`, not `overflow-hidden`: a too-wide row (the grid footer with the chat open) must never scroll
       the pane when a dialog hands focus back to a button, which cut off the left edge. -->
  <main bind:this={mainEl} class="card relative flex min-w-[300px] flex-1 flex-col overflow-clip">
    <!-- Production work gets a red top edge on every view of the tab: table data, properties and the editor. -->
    {#if ui.page === 'query' && hasConn && conn.env === 'production'}<div class="pointer-events-none absolute inset-x-0 top-0 z-sticky h-0.5 bg-danger" aria-hidden="true"></div>{/if}
    <!-- The workspace stays mounted behind full pages so editor undo and grid state survive a visit. -->
    <div id="workspace-panel" class="flex min-h-0 flex-1 flex-col" role={hasConn ? 'tabpanel' : undefined} aria-labelledby={hasConn ? `query-tab-${tab.id}` : undefined} hidden={ui.page !== 'query'}>
    {#if !hasConn}
      <div class="grid flex-1 place-items-center p-6 text-center">
        <div class="max-w-xs space-y-2">
          <div class="mx-auto grid size-9 place-items-center rounded-lg bg-accent/12 text-accent"><Database size={15} /></div>
          <p class="font-medium">{db.loading ? 'Loading connections…' : 'No connections yet'}</p>
          {#if db.loadError}<p class="text-sm text-danger" role="alert">{db.loadError}</p>{/if}
          {#if !db.loading}<button class="btn btn-primary mx-auto" onclick={newConnection}><Plus size={13} /> New connection</button>{/if}
        </div>
      </div>
    {:else if tab.table}
    {@const t = tab.table}
    {@const redis = isRedis(conn)}
    <div class="flex h-pane-header shrink-0 items-center gap-2 px-3">
      {#if t.kind === 'view'}<Eye size={13} class="shrink-0 text-accent" />{:else if t.kind === 'matview'}<ScanEye size={13} class="shrink-0 text-accent" />{:else}<Table2 size={13} class="shrink-0 text-accent" />{/if}
      <span class="min-w-0 truncate text-base font-semibold" title={displayName(t)}>{displayName(t)}</span>
      {#if conn.env === 'production' || conn.envLabel}<EnvironmentBadge {conn} />{/if}
      <span class="hidden min-w-0 truncate text-sm text-faint md:inline">{conn.name} / {redis ? `db${t.database}` : database}</span>
      <div class="seg ml-2 shrink-0" role="tablist" aria-label={redis ? 'Key view' : 'Table view'}>
        <button class="seg-item {tab.view === 'data' ? 'seg-on' : ''}" role="tab" aria-selected={tab.view === 'data'} onclick={() => { tab.view = 'data'; if (redis && !tab.result) void loadTableData(tab); }}>Data</button>
        <button class="seg-item {tab.view === 'properties' ? 'seg-on' : ''}" role="tab" aria-selected={tab.view === 'properties'} onclick={() => (tab.view = 'properties')}>Properties</button>
      </div>
      <div class="flex-1"></div>
      {#if tab.view === 'data'}
        {#if busy}
          <span class="font-mono text-xs text-muted tabular-nums">{((now - busy.started) / 1000).toFixed(1)} s</span>
          <button class="btn btn-secondary" onclick={cancel} title="Cancel the running query"><Square size={12} class="fill-current" /> Cancel</button>
        {:else}
          <button class="btn btn-ghost" onclick={() => loadTableData(tab)} title="Load the rows again"><RotateCw size={13} /> Refresh</button>
        {/if}
      {/if}
    </div>
    {#if tab.view === 'data'}
      {@const key = tableLabel(t)}
      {@const columns = tab.result?.columns.length ? tab.result.columns : (schema[key] ?? [])}
      {#key tab.id}
        <div class="flex min-h-0 flex-1 flex-col border-t border-line">
          {#if !redis}<TableFilterBar engine={conn.engine} {columns} fields={(schema[key]?.length ? schema[key] : undefined) ?? columns} types={tab.result?.types ?? []} bind:filters={tab.filters} bind:mongoFilter={tab.mongoFilter} loading={!!busy} onapply={() => loadTableData(tab)} />{/if}
          <div class="relative min-h-0 flex-1">
            <ResultsGrid tab={tab} running={busy} oncancel={() => cancelRun(tab)} sort={tab.sort ?? null} onsort={(s) => sortResult(tab, s)} onloadmore={() => loadMore(tab)} onsaved={() => { structureChanged(tab.resultConn); refreshResult(tab); }} foreignKeys={tableFks} onfollow={(fk, row) => followForeignKey(conn, t, fk, row)} />
          </div>
        </div>
      {/key}
    {:else}
      <div class="flex min-h-0 flex-1 border-t border-line">
        {#key tab.id}<TableProperties {conn} table={t} onopen={(r) => openTableView(conn, r)} />{/key}
      </div>
    {/if}
    {:else}
    <div class="flex min-h-pane-header shrink-0 flex-wrap items-center gap-x-1.5 gap-y-0.5 px-2.5 py-1.5">
      <div class="flex min-w-52 flex-1 items-center gap-1.5 xl:min-w-72">
      <TargetBar {conn} {database} {switching} onconn={(conn) => switchTarget({ conn })} ondb={(database) => switchTarget({ database })} />
      {#if manualCommit(conn)}
        {@const st = tab.status}
        <!-- An aborted transaction counts as pending; Rollback is the way out of it. Disabled buttons do not show tooltips reliably, so the wrappers carry the title. -->
        <div class="flex w-18 shrink-0 items-center gap-1 border-l border-line pl-1.5 xl:w-41">
          <span class="flex-1" title="Commit the tab's open transaction"><button class="btn btn-ghost w-full px-1.5" onclick={() => endActive(true)} disabled={!st?.txOpen || st.aborted || !!busy || txButtons.busy || !!switching} title="Commit the tab's open transaction" aria-label="Commit"><Check size={13} /> <span class="hidden xl:inline">Commit</span></button></span>
          <span class="flex-1" title="Roll back the tab's open transaction"><button class="btn btn-ghost w-full px-1.5" onclick={() => endActive(false)} disabled={!st?.txOpen || !!busy || txButtons.busy || !!switching} title="Roll back the tab's open transaction" aria-label="Rollback"><Undo2 size={13} /> <span class="hidden xl:inline">Rollback</span></button></span>
        </div>
        {#if st?.txOpen}
          <span class="flex min-w-0 items-center gap-1.5 text-sm font-medium text-warn" role="status" title="Transaction open"><span class="size-1.5 shrink-0 rounded-full bg-warn"></span> <span class="truncate">Transaction open</span></span>
        {/if}
        {#if txButtons.error?.tab === tab.id}<span class="min-w-0 truncate text-sm text-danger" role="alert">{txButtons.error.text}</span>{/if}
      {/if}
      </div>
      <div class="ml-auto flex min-w-0 items-center gap-1.5">
      <!-- statementKind reads SQL keywords, so only SQL tabs have a Read/Write cue. -->
      {#if isSql(conn)}
        {#if kind === 'read'}
          <span class="hidden shrink-0 px-1 text-sm font-medium text-ok lg:inline" role="status" title="This query only reads data">Read</span>
        {:else if kind === 'write'}
          <span class="shrink-0 px-1 text-sm font-medium text-warn" role="status" title="This query changes data">Write</span>
        {/if}
      {/if}
      <!-- A saved, unchanged tab shows "Saved"; ⌘S then has nothing to write. -->
      <button class="btn btn-ghost shrink-0 px-2 xl:px-2.5" onclick={() => saveQuery()} disabled={!savedQ && kind === 'empty'} title={savedQ ? (tabClean ? `“${savedQ.name}” is saved · ${KEYS.saveAs.hint} saves a copy` : `Save changes to “${savedQ.name}” (${KEYS.save.hint})`) : `Save query (${KEYS.save.hint})`} aria-label={tabClean ? 'Saved' : 'Save query'}>
        {#if tabClean}<Check size={13} class="text-ok" />{:else}<Save size={13} />{/if}
        <span class="hidden xl:inline">{tabClean ? 'Saved' : 'Save'}</span>
      </button>
      <!-- Opens the chat on this tab's database. With the chat already open, narrow toolbars drop it. -->
      <button class="btn btn-ghost shrink-0 px-2 2xl:px-2.5 {ui.chatOpen ? 'hidden lg:inline-flex' : ''}" onclick={openChatOnTab} title="Explain with AI" aria-label="Explain with AI"><Sparkles size={13} /> <span class="hidden 2xl:inline">Explain with AI</span></button>
      {#if busy}
        {#if busy.script}
          {@const s = busy.script}
          <!-- Total is 0 until the first poll answers. -->
          <span class="flex min-w-0 items-center gap-2 text-sm text-muted" role="status" title="Running {s.name}">
            <span class="max-w-40 truncate">{s.name}</span>
            <span class="h-1 w-12 shrink-0 overflow-hidden rounded-full bg-hover xl:w-16" role="progressbar" aria-label="Script progress" aria-valuenow={s.done} aria-valuemin={0} aria-valuemax={s.total || undefined}>
              <span class="block h-full rounded-full bg-accent motion-safe:transition-[width] motion-safe:duration-150" style:width="{s.total ? (s.done / s.total) * 100 : 0}%"></span>
            </span>
            <span class="shrink-0 font-mono text-xs tabular-nums">{s.total ? `${s.done}/${s.total}` : '…'}</span>
          </span>
        {/if}
        <span class="font-mono text-xs text-muted tabular-nums">{((now - busy.started) / 1000).toFixed(1)} s</span>
        <button class="btn btn-secondary" onclick={cancel} title="Cancel the running query"><Square size={12} class="fill-current" /> Cancel</button>
      {:else}
        <button class="btn btn-primary shrink-0 pr-2" onclick={() => run({ sql: currentSql() })} disabled={kind === 'empty' || !!switching} title="Run the selection, or the statement at the cursor"><Play size={12} class="fill-current" /> Run <kbd class="ml-0.5 hidden font-sans text-xs font-normal xl:inline" aria-hidden="true">{KEYS.run.hint}</kbd></button>
      {/if}
      </div>
    </div>

    <div bind:this={panesEl} class="flex min-h-0 flex-1 flex-col">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="overflow-hidden {ui.resultsOpen ? 'shrink-0' : 'min-h-0 flex-1'}" style:min-height={ui.resultsOpen ? 'min(7rem, 25%)' : undefined} style:max-height={ui.resultsOpen ? 'calc(100% - min(10rem, 75%))' : undefined} style:height={ui.resultsOpen ? `${ui.editorSplit * 100}%` : undefined} oncontextmenu={openEditorMenu}>
      <!-- Keyed so undo never crosses into another tab's text. -->
      {#key tab.id}
        <SqlEditor bind:this={editor} bind:value={tab.query} onrun={() => { if (!switching) void run({ sql: currentSql() }); }} onrunnew={() => { if (!switching) void run({ sql: currentSql(), append: true }); }} onrunall={() => { if (!switching) void run({ sql: tab.query }); }} onformat={formatQuery} snapshot={snapItems} {tablesState} {columnsOf} {linksOf} dialect={conn.engine} />
      {/key}
    </div>
    <input bind:this={scriptInput} type="file" accept=".sql,text/plain" class="hidden" onchange={runScript} />

    {#snippet windowControls()}
      <button class="btn btn-sm btn-ghost btn-on" aria-pressed={ui.resultsOpen} aria-controls="query-results" title={ui.resultsOpen ? 'Hide results' : 'Show results'} onclick={toggleResults} data-results-toggle><PanelBottom size={13} /> Results</button>
      {#if ui.resultsOpen}
        <button class="btn btn-sm btn-ghost size-6 p-0" title="Maximize results / restore" aria-label="Move split up" onclick={() => sash('up')}><ChevronUp size={12} /></button>
        <button class="btn btn-sm btn-ghost size-6 p-0" title="Maximize editor / restore" aria-label="Move split down" onclick={() => sash('down')}><ChevronDown size={12} /></button>
        {#if tab.results.length === 1 && tab.result}
          <button class="btn btn-sm btn-ghost size-6 p-0" title="Close result" aria-label="Close result" onclick={() => tab.result && dropResult(tab, tab.result)}><X size={12} /></button>
        {/if}
      {/if}
    {/snippet}

    <!-- Hidden, not unmounted, so the grid keeps its scroll, selection and edit mode. -->
    <div class="contents" hidden={!ui.resultsOpen}>
      <!-- The sash overlaps its neighboring panes, so the 8px grab zone is wider than the grab line. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
      <div class="group relative z-sticky -my-1 grid h-2 shrink-0 cursor-row-resize touch-none place-items-center outline-none hover:bg-accent/15 focus-visible:bg-accent/25" role="separator" aria-orientation="horizontal" aria-label="Resize editor" aria-controls="query-results" aria-valuenow={Math.round(ui.editorSplit * 100)} aria-valuemin={10} aria-valuemax={80} tabindex="0" onpointerdown={resizeEditor} onkeydown={keyEditor}>
        <span class="pointer-events-none h-[3px] w-8 rounded-full bg-faint/65 group-hover:bg-accent group-focus-visible:bg-accent" aria-hidden="true"></span>
      </div>

      <div class="flex min-h-0 flex-1 flex-col border-t border-line" id="query-results">
        <!-- Result tab strip (as in DBeaver): one tab per grid result; its X closes that result. -->
        {#if tab.results.length > 1}
          <div class="flex h-8 shrink-0 items-end gap-0.5 overflow-x-auto border-b border-line px-1.5" role="tablist" aria-label="Results">
            {#each tab.results as r, i (r)}
              {@const on = tab.result === r}
              <div role="presentation" class="-mb-px flex h-7 shrink-0 items-center gap-1.5 rounded-t-md border border-b-0 pr-1 text-sm font-medium {on ? 'border-line bg-surface text-ink' : 'border-transparent text-muted hover:bg-hover hover:text-ink'}">
                <button class="flex h-full items-center gap-1.5 rounded-t-md pl-2.5" role="tab" aria-selected={on} title={resultTip(r, i)} onclick={() => openResult(tab, r)}>
                  {#if r.title === 'Messages'}<ScrollText size={13} class="shrink-0 text-accent" />{:else}<Table2 size={13} class="shrink-0 text-accent" />{/if}
                  <span class="max-w-40 truncate">{r.title ?? `Result ${i + 1}`}</span>
                </button>
                <button class="focus-ring ml-1 grid size-[18px] place-items-center rounded text-faint hover:bg-hover hover:text-ink" title="Close result" aria-label="Close result" onclick={() => dropResult(tab, r)}><X size={12} /></button>
              </div>
            {/each}
        </div>
        {/if}
        <div class="relative min-h-0 flex-1">
          {#key tab.id}
            <ResultsGrid tab={tab} {windowControls} running={busy} oncancel={() => cancelRun(tab)} sort={tab.result?.sort ?? null} onsort={(s) => sortResult(tab, s)} onloadmore={() => loadMore(tab)} onsaved={() => { structureChanged(tab.resultConn); refreshResult(tab); }} />
          {/key}
        </div>
      </div>
    </div>

    {#if !ui.resultsOpen}
      <GridToolbar summary={tab.result} {windowControls} />
    {/if}
    </div>

    {/if}
    </div>
    {#if ui.page !== 'query'}
      {#key ui.page}
        <div class="absolute inset-0 bg-surface" transition:fade|global={{ duration: matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : 150 }} onoutrostart={(e) => (e.currentTarget.inert = true)} onintrostart={(e) => (e.currentTarget.inert = false)}>
          {#if ui.page === 'settings'}
            <SettingsPage onEnableWrites={enableWrites} />
          {:else if ui.page === 'audit'}
            <AuditPage />
          {:else}
            <SchedulerPage />
          {/if}
        </div>
      {/key}
    {/if}
    <NoticeToast />
  </main>

  <!-- Both positions use the same mounted panel; the dock reserves space only while it is open. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="-mx-2 w-2 shrink-0 cursor-col-resize touch-none rounded-full outline-none hover:bg-accent/25 focus-visible:bg-accent/40" role="separator" aria-orientation="vertical" aria-label="Resize assistant" aria-controls="assistant-panel" aria-valuenow={chatWidth} aria-valuemin={240} aria-valuemax={chatWidthLimit} tabindex="0" hidden={!ui.chatOpen || !dockChat} onpointerdown={resizeChat} onkeydown={keyChat}></div>
  <div class="assistant-slot {dockChat ? 'assistant-docked relative shrink-0' : 'absolute inset-y-0 right-2'} {resizingChat ? '' : 'motion-safe:transition-[width] motion-safe:duration-[160ms]'}" style:--assistant-width="{chatWidth}px" hidden={chatHidden}>
    <div bind:this={chatPanel} id="assistant-panel" class="assistant-panel absolute inset-y-0 right-0 z-chat {dockChat ? '' : 'bottom-2 rounded-[10px] shadow-2xl'} motion-safe:transition-transform motion-safe:duration-[160ms]" inert={!ui.chatOpen}>
      <ChatPanel {tables} activeBusy={!!busy} onEnableWrites={enableWrites} onInsertSql={chatInsert} onRunSql={chatRun} />
    </div>
  </div>
  </div>
</div>
<svelte:window bind:innerWidth={viewportWidth} oncontextmenu={blockNativeMenu} />
{#if tabMenu}
  {@const m = tabMenu}
  <Menu anchor={m.anchor} returnFocus={m.trigger} label="Tab actions" onclose={() => (tabMenu = null)}>
    <button class="menu-item" role="menuitem" tabindex="-1" onclick={() => (renaming = m.t.id)}><Pencil size={13} /> Rename</button>
    <button class="menu-item" role="menuitem" tabindex="-1" onclick={() => dropTab(m.t)}><X size={13} /> Close</button>
    <button class="menu-item" role="menuitem" tabindex="-1" disabled={tabs.list.length < 2} onclick={() => closeOtherTabs(m.t)}><Copy size={13} /> Close others</button>
  </Menu>
{/if}
{#if editorMenu}
  {@const m = editorMenu}
  <Menu anchor={{ x: m.x, y: m.y }} label="Query editor actions" class="w-64" onclose={() => (editorMenu = null)}>
    <button class="menu-item" role="menuitem" tabindex="-1" disabled={!!busy || !!switching} onclick={() => { const cmd = editor?.commands(); if (cmd) void run({ sql: m.sel ? cmd.selection() : cmd.statementAt() }); }}><Play size={13} /> <span class="flex-1">{m.sel ? 'Run selection' : 'Run statement at cursor'}</span><kbd class="kbd">{KEYS.run.hint}</kbd></button>
    <button class="menu-item" role="menuitem" tabindex="-1" disabled={!!busy || !!switching || kind === 'empty'} onclick={() => run({ sql: tab.query })}><ListOrdered size={13} /> <span class="flex-1">Run all statements</span><kbd class="kbd">{KEYS.runAll.hint}</kbd></button>
    {#if !isRedis(conn)}
      <button class="menu-item" role="menuitem" tabindex="-1" disabled={!!busy || !!switching} onclick={() => scriptInput?.click()}><FileUp size={13} /> {isSql(conn) ? 'Run SQL script from file…' : 'Run script from file…'}</button>
      <button class="menu-item" role="menuitem" tabindex="-1" disabled={kind === 'empty'} onclick={formatQuery}><WandSparkles size={13} /> <span class="flex-1">{m.sel ? 'Format selection' : 'Format query'}</span><kbd class="kbd">{KEYS.format.hint}</kbd></button>
    {/if}
    <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
    <button class="menu-item" role="menuitem" tabindex="-1" disabled={!savedQ && kind === 'empty'} onclick={() => saveQuery()}><Save size={13} /> <span class="flex-1">{savedQ ? 'Save changes' : 'Save query…'}</span><kbd class="kbd">{KEYS.save.hint}</kbd></button>
    <button class="menu-item" role="menuitem" tabindex="-1" disabled={kind === 'empty'} onclick={() => saveQuery(true)}><CopyPlus size={13} /> <span class="flex-1">Save as new query…</span><kbd class="kbd">{KEYS.saveAs.hint}</kbd></button>
    <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
    <button class="menu-item" role="menuitem" tabindex="-1" disabled={!m.sel} onclick={() => copySelection(true)}><Scissors size={13} /> Cut</button>
    <button class="menu-item" role="menuitem" tabindex="-1" disabled={!m.sel} onclick={() => copySelection(false)}><Copy size={13} /> Copy</button>
    <button class="menu-item" role="menuitem" tabindex="-1" onclick={async () => { const target = editor; const text = await navigator.clipboard?.readText().catch(() => undefined); if (text !== undefined && target === editor) target?.commands().insert(text); }}><ClipboardPaste size={13} /> Paste</button>
    <button class="menu-item" role="menuitem" tabindex="-1" onclick={() => editor?.commands().selectAll()}><span class="size-[13px]"></span> Select all</button>
  </Menu>
{/if}
<ConnectionDialog bind:open={overlays.connection} conn={connEdit} canswitch={canSwitch} canupdate={canUpdate} onsaved={openFirstTable} onedited={connEdited} />
<WhatsNewDialog />
<CommandPalette
  bind:open={overlays.palette}
  tables={paletteTables}
  onfocuseditor={focusEditor}
  onrun={() => { if (kind === 'empty' || switching) return; ui.page = 'query'; run({ sql: currentSql() }); }}
  ontogglechat={() => (ui.chatOpen = !ui.chatOpen)}
  ontogglesidebar={() => (ui.sidebarOpen = !ui.sidebarOpen)}
  onpage={(p) => (ui.page = p)}
  ontoggletheme={toggleTheme}
  onnewconnection={newConnection}
  onswitch={(c) => { ui.page = 'query'; switchTarget({ conn: c }); }}
  onsave={() => saveQuery()}
  onopensaved={(q) => openSaved(q)}
/>
<!-- Last, so an answer stacks above the connection dialog and any dialog that waits on it. -->
<AskHost />

<style>
  .assistant-slot,
  .assistant-panel {
    width: var(--assistant-width);
  }

  .assistant-docked:has(.assistant-panel[inert]) { width: 0; margin-left: -0.5rem; }
  .assistant-panel[inert] { translate: 100% 0; pointer-events: none; }

  @starting-style {
    .assistant-docked { width: 0; }
    .assistant-panel { translate: 100% 0; }
  }
</style>
