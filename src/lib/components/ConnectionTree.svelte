<script lang="ts">
  import { ChevronRight, Table2, Plus, Layers, KeyRound, Columns2, Database, Pencil, Copy, Trash, Ellipsis, LoaderCircle, RotateCw, Eye, ScanEye, ListTree, Lock, Hash, FileCode, FileSearch, FilePlus, Download, Sheet, Eraser } from '@lucide/svelte';
  import { untrack } from 'svelte';
  import { db, active, engines, envDot, envTag, canWrite, isSql, type Conn } from '../state.svelte';
  import { schemas, nsOf, pkOf, colType } from '../mock';
  import { api, ApiError, type ColumnInfo, type TableInfo, type TableKind } from '../api';
  import { displayName, kindLabel, type TableAction, type TableRef } from '../tableActions';

  let {
    selected,
    onopen,
    onnew,
    onedit,
    onduplicate,
    ondelete,
    ontable,
    onreorder,
  }: {
    selected: string | null;
    /** Double-click, ⌘/Ctrl-click or Enter on a table: open it in a table tab. A single click only selects it. On MongoDB this opens the collection's data view. */
    onopen: (c: Conn, t: TableRef) => void;
    onnew: () => void;
    onedit: (c: Conn) => void;
    onduplicate: (c: Conn) => void;
    ondelete: (c: Conn) => void;
    ontable: (a: TableAction, c: Conn, t: TableRef) => void;
    /** A connection row was dragged (or moved with Alt+↑/↓) to a new place. `ids`: the full new order. */
    onreorder: (ids: string[]) => void;
  } = $props();

  // A single click marks a table row; the highlight then follows the active tab again when it changes.
  let picked = $state<string | null>(null);
  $effect(() => {
    selected;
    picked = null;
  });
  const hl = $derived(picked ?? selected);

  function tableClick(e: MouseEvent, key: string, c: Conn, t: TableRef) {
    // ⌘/Ctrl-click opens a table data tab; on MongoDB it runs the collection's find().
    if (e.metaKey || e.ctrlKey) onopen(c, t);
    else picked = key;
  }
  function tableKey(e: KeyboardEvent, c: Conn, t: TableRef) {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    onopen(c, t);
  }

  let menu = $state<{ x: number; y: number; c: Conn } | null>(null);

  function openMenu(e: MouseEvent, c: Conn) {
    e.preventDefault();
    e.stopPropagation();
    // A context menu opened from the keyboard reports (0, 0); place it under the row instead.
    const r = e.clientX === 0 && e.clientY === 0 ? (e.currentTarget as HTMLElement).getBoundingClientRect() : null;
    tableMenu = null;
    menu = place(r ? r.left + 16 : e.clientX, r ? r.bottom + 2 : e.clientY, c);
  }

  function toggleMenuFromButton(e: MouseEvent & { currentTarget: HTMLElement }, c: Conn) {
    e.stopPropagation();
    if (menu?.c.id === c.id) return (menu = null);
    const r = e.currentTarget.getBoundingClientRect();
    menu = place(r.left, r.bottom + 2, c);
  }

  // Menu is w-44 (176px) and about 124px tall; keep it inside the window.
  const place = (x: number, y: number, c: Conn) => ({ x: Math.max(4, Math.min(x, innerWidth - 180)), y: Math.max(4, Math.min(y, innerHeight - 132)), c });

  function act(fn: (c: Conn) => void) {
    const c = menu?.c;
    menu = null;
    if (c) fn(c);
  }

  // Drag a connection row to sort the list. Pointer events, not HTML5 drag and drop: the Tauri
  // window's own file-drop handling can swallow HTML5 drag events. `to`: the gap the row goes to,
  // 0 = above the first row, n = below the last. The drag starts after 4px, so a click still toggles.
  let drag = $state<{ id: string; y0: number; on: boolean; to: number } | null>(null);
  let dragged = false;

  function dragStart(e: PointerEvent, c: Conn) {
    if (e.button !== 0 || e.metaKey || e.ctrlKey) return;
    drag = { id: c.id, y0: e.clientY, on: false, to: 0 };
  }

  function dragMove(e: PointerEvent) {
    if (!drag) return;
    if (!drag.on && Math.abs(e.clientY - drag.y0) < 4) return;
    drag.on = true;
    menu = tableMenu = null;
    const rows = document.querySelectorAll<HTMLElement>('[data-conn-row]');
    let to = 0;
    for (const r of rows) {
      const b = r.getBoundingClientRect();
      if (e.clientY > b.top + b.height / 2) to++;
    }
    drag.to = to;
  }

  function dragEnd() {
    const d = drag;
    drag = null;
    if (!d?.on) return;
    // The click that follows this pointerup must not toggle the row.
    dragged = true;
    setTimeout(() => (dragged = false));
    move(d.id, d.to);
  }

  /** Moves connection `id` to gap `to` (see `drag`). */
  function move(id: string, to: number) {
    const ids = db.connections.map((c) => c.id);
    const from = ids.indexOf(id);
    if (from < 0 || to < 0 || to > ids.length || to === from || to === from + 1) return;
    ids.splice(from, 1);
    ids.splice(to > from ? to - 1 : to, 0, id);
    onreorder(ids);
  }

  function rowKey(e: KeyboardEvent, c: Conn, i: number) {
    if (!e.altKey || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
    e.preventDefault();
    move(c.id, e.key === 'ArrowUp' ? i - 1 : i + 2);
  }

  // Table menu. `list`: the tree key of the table list, so Refresh reloads it.
  let tableMenu = $state<{ x: number; y: number; c: Conn; t: TableRef; list: string | null } | null>(null);

  function openTableMenu(e: MouseEvent, c: Conn, t: TableRef, list: string | null) {
    e.preventDefault();
    e.stopPropagation();
    menu = null;
    const r = e.clientX === 0 && e.clientY === 0 ? (e.currentTarget as HTMLElement).getBoundingClientRect() : null;
    // Menu is w-60 (240px) and about 470px tall; keep it inside the window.
    tableMenu = { x: Math.max(4, Math.min(r ? r.left + 16 : e.clientX, innerWidth - 244)), y: Math.max(4, Math.min(r ? r.bottom + 2 : e.clientY, innerHeight - 474)), c, t, list };
  }

  function tableAct(a: TableAction) {
    const m = tableMenu;
    tableMenu = null;
    if (m) ontable(a, m.c, m.t);
  }

  function copyName() {
    const m = tableMenu;
    tableMenu = null;
    if (m) navigator.clipboard?.writeText(displayName(m.t)).catch(() => {});
  }

  // Files are written by the desktop app only.
  const canExport = api.kind === 'tauri';

  /** Reloads the table list that holds `t`, e.g. after a drop. */
  export function refreshTables(c: Conn, t: TableRef) {
    if (!isMock || !isSql(c)) refresh(t.schema === null ? `${c.id}/${t.database}` : `${c.id}/${t.database}/${t.schema}`);
  }

  // Node keys that hold a table list, and where that list comes from.
  const tableLists = $state<Record<string, { connId: string; database: string; schema: string | null }>>({});

  /** Every table the tree has loaded, hidden partitions included, for the palette search. Partitions come last. */
  export function loadedTables(): { c: Conn; t: TableRef; partition: boolean }[] {
    const out: { c: Conn; t: TableRef; partition: boolean }[] = [];
    for (const [k, l] of Object.entries(tableLists)) {
      const c = db.connections.find((x) => x.id === l.connId);
      if (!c || nodes[k]?.status !== 'ok') continue;
      for (const t of tableList(k)) out.push({ c, t: { database: l.database, schema: l.schema, name: t.name, kind: t.kind }, partition: !!t.partitionOf });
    }
    return out.sort((a, b) => +a.partition - +b.partition);
  }

  // Keys: connection id, `id/db`, `id/db/schema` (pg only), then `…/table`. Kept for the session only.
  const open = $state<Record<string, boolean>>({});

  function groups(c: Conn) {
    const out: Record<string, string[]> = {};
    for (const t of Object.keys(schemas[c.schema])) (out[nsOf(c.engine, t)] ??= []).push(t);
    return Object.entries(out);
  }

  // A connection opened for the first time also opens its database and, on pg, its first schema.
  // Mock SQL connections browse the local prototype data; a mock MongoDB connection uses the API
  // path like a real one, because the mock backend answers its browse calls.
  function expand(c: Conn) {
    if (!isMock || !isSql(c)) return expandReal(c);
    open[c.id] = true;
    const dk = `${c.id}/db`;
    if (open[dk] === undefined) open[dk] = true;
    if (c.engine !== 'pg') return;
    const first = `${dk}/${groups(c)[0]?.[0]}`;
    if (open[first] === undefined) open[first] = true;
  }

  // The active connection starts expanded; also applies after a switch or a new connection.
  $effect(() => {
    const c = db.connections.find((x) => x.id === active().id);
    if (c && open[c.id] === undefined) untrack(() => expand(c));
  });

  const toggle = (k: string) => (open[k] = !open[k]);
  const pad0 = 6;
  /** `extra`: rows without an inline chevron (tables, columns) add the chevron and gap width. */
  const pad = (level: number, extra = 0) => `padding-left:${pad0 + level * 12 + extra}px`;
  // Indent-guide x, through the chevron center of the row at `level`.
  const guide = (level: number) => pad0 + level * 12 + 5.5;
  /** One 1px vertical guide per open ancestor level, like VS Code's tree. */
  const guides = (levels: number[]) => levels.map((l) => `linear-gradient(var(--c-hover),var(--c-hover)) ${guide(l)}px 0/1px 100% no-repeat`).join(',');
  /** The connection's own guide: a red 2px rail on production, so every row under it shows where it lives. */
  const rootGuide = (c: Conn) => (c.env === 'production' ? `linear-gradient(#dc2626b3,#dc2626b3) ${guide(0) - 0.5}px 0/2px 100% no-repeat` : guides([0]));

  // ---- lazy introspection through the API: every real backend, and MongoDB in mock mode ----
  const isMock = api.kind === 'mock';
  type Node = { status: 'loading' | 'ok' | 'error'; items: unknown[]; error: string };
  // Keys: `<id>` databases, `<id>/<db>` schemas (pg) or tables, `<id>/<db>/<schema>` pg tables, then `…/<table>` columns.
  // Open state reuses these keys with an `o:` prefix.
  const nodes = $state<Record<string, Node>>({});
  const loaders: Record<string, () => Promise<unknown[]>> = {};
  const seq: Record<string, number> = {};

  async function load(key: string, fn: () => Promise<unknown[]>) {
    loaders[key] = fn;
    const n = (seq[key] = (seq[key] ?? 0) + 1);
    nodes[key] = { status: 'loading', items: nodes[key]?.items ?? [], error: '' };
    try {
      const items = await fn();
      if (seq[key] === n) nodes[key] = { status: 'ok', items, error: '' };
    } catch (e) {
      if (seq[key] === n) nodes[key] = { status: 'error', items: [], error: ApiError.from(e).message };
    }
  }
  const ensure = (key: string, fn: () => Promise<unknown[]>) => (nodes[key] ? Promise.resolve() : load(key, fn));
  const strings = (key: string) => (nodes[key]?.items ?? []) as string[];
  const tableList = (key: string) => (nodes[key]?.items ?? []) as TableInfo[];
  const colList = (key: string) => (nodes[key]?.items ?? []) as ColumnInfo[];

  // Tables, views and materialized views are separate folders; each shows PAGE rows, then Load more.
  const PAGE = 100;
  const kindGroups: { kind: TableKind; label: string; icon: typeof Table2 }[] = [
    { kind: 'table', label: 'Tables', icon: Table2 },
    { kind: 'view', label: 'Views', icon: Eye },
    { kind: 'matview', label: 'Materialized views', icon: ScanEye },
  ];
  const kindIcon = Object.fromEntries(kindGroups.map((g) => [g.kind, g.icon])) as Record<TableKind, typeof Table2>;
  // Rows shown per folder, by `<list key>#<kind>`. Absent: PAGE.
  const shown = $state<Record<string, number>>({});

  // PG partitions stay out of the tree unless the user shows them. One choice for all connections, kept across restarts.
  const PARTITIONS_KEY = 'sandb.tree.show-partitions';
  let showPartitions = $state(localStorage.getItem(PARTITIONS_KEY) === '1');
  function setShowPartitions(on: boolean) {
    showPartitions = on;
    localStorage.setItem(PARTITIONS_KEY, on ? '1' : '0');
  }
  const visible = (list: TableInfo[]) => (showPartitions ? list : list.filter((t) => !t.partitionOf));

  /** After a structure change: reloads the connection's databases and every loaded list below them.
   *  The open state is untouched, so expanded nodes stay open. */
  export function refreshStructure(c: Conn) {
    refresh(c.id);
  }

  /** Whether the tree already lists this collection. A Mongo write into an unlisted name may create it. */
  export function hasCollection(c: Conn, database: string, collection: string): boolean {
    const key = `${c.id}/${database}`;
    if (nodes[key]?.status !== 'ok') return false;
    return tableList(key).some((t) => t.name === collection);
  }

  /** Reloads a node and every loaded node below it. */
  function refresh(key: string) {
    for (const k of Object.keys(nodes).sort((a, b) => a.length - b.length)) {
      if ((k === key || k.startsWith(key + '/')) && loaders[k]) load(k, loaders[k]);
    }
  }

  /** `auto`: on pg, also open `public` (or the first schema) once the schemas are in. */
  async function openDb(c: Conn, d: string, auto = false) {
    const k = `${c.id}/${d}`;
    open['o:' + k] = true;
    if (c.engine !== 'pg') {
      tableLists[k] = { connId: c.id, database: d, schema: null };
      return ensure(k, () => api.listTables(c.id, d, null));
    }
    await ensure(k, () => api.listSchemas(c.id, d));
    const list = strings(k);
    const s = list.includes('public') ? 'public' : list[0];
    if (auto && s && open[`o:${k}/${s}`] === undefined) openSchema(c, d, s);
  }

  function openSchema(c: Conn, d: string, s: string) {
    const k = `${c.id}/${d}/${s}`;
    open['o:' + k] = true;
    tableLists[k] = { connId: c.id, database: d, schema: s };
    ensure(k, () => api.listTables(c.id, d, s));
  }

  function toggleCols(c: Conn, d: string, s: string | null, t: string, k: string) {
    open['o:' + k] = !open['o:' + k];
    if (open['o:' + k]) ensure(k, () => api.listColumns(c.id, d, s, t));
  }

  // First expand loads the databases and opens the connection's own one (when it names one).
  function expandReal(c: Conn) {
    open[c.id] = true;
    ensure(c.id, () => api.listDatabases(c.id));
    const own = c.engine === 'sqlite' ? 'main' : c.database;
    if (own && open[`o:${c.id}/${own}`] === undefined) openDb(c, own, true);
  }

  // An edit that points a connection elsewhere makes its loaded tree stale.
  const sigs: Record<string, string> = {};
  $effect(() => {
    for (const c of db.connections) {
      // In the mock, SQL connections browse the fixed prototype data; MongoDB goes through the API.
      if (isMock && isSql(c)) continue;
      const sig = [c.engine, c.host, c.port, c.database, c.user, c.file].join('|');
      if (sigs[c.id] !== undefined && sigs[c.id] !== sig) untrack(() => forget(c));
      sigs[c.id] = sig;
    }
  });

  function forget(c: Conn) {
    const mine = (k: string) => k === c.id || k.startsWith(c.id + '/');
    for (const k of Object.keys(nodes)) if (mine(k)) (delete nodes[k], delete loaders[k], seq[k]++);
    for (const k of Object.keys(shown)) if (mine(k)) delete shown[k];
    const wasOpen = open[c.id];
    for (const k of Object.keys(open)) if (mine(k.replace(/^o:/, ''))) delete open[k];
    if (wasOpen) expandReal(c);
  }</script>

{#snippet status(key: string, level: number, empty: boolean)}
  {@const n = nodes[key]}
  {#if n?.status === 'loading' && !n.items.length}
    <div class="flex h-[26px] items-center gap-2 text-[12px] text-faint" style={pad(level, 20)}><LoaderCircle size={12} class="animate-spin" /> Loading…</div>
  {:else if n?.status === 'error'}
    <div class="flex h-[26px] items-center gap-1 pr-1 text-[12px] text-red-700 dark:text-red-400" style={pad(level, 20)} role="alert">
      <span class="min-w-0 flex-1 truncate" title={n.error}>{n.error}</span>
      <button class="grid size-5 shrink-0 place-items-center rounded hover:bg-hover" title="Retry" aria-label="Retry" onclick={() => refresh(key)}><RotateCw size={12} /></button>
    </div>
  {:else if n?.status === 'ok' && !n.items.length && empty}
    <div class="flex h-[26px] items-center text-[12px] text-faint" style={pad(level, 20)}>Empty</div>
  {/if}
{/snippet}

{#snippet refreshBtn(key: string, label: string)}
  <button
    class="absolute top-[3px] right-1 grid size-5 place-items-center rounded text-faint opacity-0 group-hover/n:opacity-100 focus-visible:opacity-100 hover:bg-hover hover:text-ink"
    title="Refresh {label}"
    aria-label="Refresh {label}"
    onclick={() => refresh(key)}
  >
    <RotateCw size={12} class={nodes[key]?.status === 'loading' ? 'animate-spin' : ''} />
  </button>
{/snippet}

{#snippet toggleBtn(level: number, isOpen: boolean, label: string, onclick: () => void)}
  <button class="absolute top-[5px] grid h-4 w-3 place-items-center rounded-sm text-faint hover:text-ink" style="left:{pad0 + level * 12}px" aria-label={label} aria-expanded={isOpen} {onclick}>
    <ChevronRight size={12} class="transition-transform {isOpen ? 'rotate-90' : ''}" />
  </button>
{/snippet}

{#snippet realTables(c: Conn, d: string, s: string | null, level: number, anc: number[])}
  {@const pk0 = s === null ? `${c.id}/${d}` : `${c.id}/${d}/${s}`}
  {@const all = tableList(pk0)}
  {@const parts = all.filter((t) => t.partitionOf).length}
  {@const shownList = visible(all)}
  <div class="relative" style="background:{guides(anc)}">
    {#each kindGroups as g (g.kind)}
      {@const items = shownList.filter((t) => t.kind === g.kind)}
      <!-- MongoDB calls its tables collections; SQL folder labels stay as they are. -->
      {@const gLabel = g.kind === 'table' && !isSql(c) ? 'Collections' : g.label}
      <!-- Partitions are tables, so the toggle lives on the Tables folder. It stays when every table is a hidden partition. -->
      {@const toggle = g.kind === 'table' && parts > 0}
      {#if items.length || toggle}
        {@const gk = `${pk0}#${g.kind}`}
        <!-- The Tables folder starts open; the others start closed. -->
        {@const gOpen = open['o:' + gk] ?? g.kind === 'table'}
        {@const n = shown[gk] ?? PAGE}
        <div class="group/n relative">
          <button class="nav-item {toggle ? 'pr-7' : ''}" style={pad(level)} aria-expanded={gOpen} title={toggle && !showPartitions ? `${parts} partitions hidden` : undefined} onclick={() => (open['o:' + gk] = !gOpen)}>
            <ChevronRight size={12} class="shrink-0 text-faint transition-transform {gOpen ? 'rotate-90' : ''}" />
            <g.icon size={13} class="shrink-0 text-faint" />
            <span class="min-w-0 flex-1 truncate">{gLabel}</span>
            <span class="text-[11px] text-faint tabular-nums {toggle ? 'group-hover/n:invisible' : ''}">{items.length}</span>
          </button>
          {#if toggle}
            {@const label = showPartitions ? 'Hide partitions' : `Show ${parts} partitions`}
            <button
              class="absolute top-[3px] right-1 grid size-5 place-items-center rounded opacity-0 group-hover/n:opacity-100 focus-visible:opacity-100 hover:bg-hover hover:text-ink {showPartitions ? 'text-accent' : 'text-faint'}"
              title={label}
              aria-label={label}
              aria-pressed={showPartitions}
              onclick={() => setShowPartitions(!showPartitions)}
            >
              <ListTree size={12} />
            </button>
          {/if}
        </div>
        {#if gOpen}
          {@render tableItems(c, d, s, pk0, items.slice(0, n), level + 1)}
          {#if items.length > n}
            <div class="relative" style="background:{guides([level])}">
              <button class="nav-item text-faint hover:text-ink" style={pad(level + 1, 20)} onclick={() => (shown[gk] = n + PAGE)}>
                <Ellipsis size={13} class="shrink-0" />
                <span class="min-w-0 flex-1 truncate">Load {Math.min(PAGE, items.length - n)} more</span>
                <span class="text-[11px] tabular-nums">{n} of {items.length}</span>
              </button>
            </div>
          {/if}
          {#if toggle && !showPartitions}
            <div class="relative" style="background:{guides([level])}">
              <button class="nav-item text-faint hover:text-ink" style={pad(level + 1, 20)} title="{parts} partitions hidden" onclick={() => setShowPartitions(true)}>
                <ListTree size={13} class="shrink-0" />
                <span class="min-w-0 flex-1 truncate">Show {parts} partitions</span>
              </button>
            </div>
          {/if}
        {/if}
      {/if}
    {/each}
  </div>
{/snippet}

{#snippet tableItems(c: Conn, d: string, s: string | null, pk0: string, items: TableInfo[], level: number)}
  <div class="relative" style="background:{guides([level - 1])}">
    {#each items as t (t.name)}
      {@const tk = `${pk0}/${t.name}`}
      {@const sel = hl === `${c.id}/${d}/${s ?? ''}/${t.name}`}
      {@const ref = { database: d, schema: s, name: t.name, kind: t.kind }}
      {@const Icon = kindIcon[t.kind]}
      <div class="relative">
        <button
          class="nav-item select-none {sel ? 'nav-item-active' : ''}"
          style={pad(level, 20)}
          title="{s ? s + '.' : ''}{t.name}{t.kind === 'table' ? '' : ` (${kindLabel[t.kind]})`}{t.partitionOf ? ` · partition of ${t.partitionOf}` : ''}{t.rowEstimate != null && t.rowEstimate >= 0 ? ` · ~${t.rowEstimate} rows` : ''}"
          onclick={(e) => tableClick(e, `${c.id}/${d}/${s ?? ''}/${t.name}`, c, ref)}
          ondblclick={() => onopen(c, ref)}
          onkeydown={(e) => tableKey(e, c, ref)}
          oncontextmenu={(e) => openTableMenu(e, c, ref, pk0)}
        >
          <Icon size={13} class="shrink-0 {sel ? 'text-accent' : 'text-faint'}" />
          <span class="min-w-0 flex-1 truncate">{t.name}</span>
        </button>
        {@render toggleBtn(level, !!open['o:' + tk], `Toggle columns of ${t.name}`, () => toggleCols(c, d, s, t.name, tk))}
      </div>
      {#if open['o:' + tk]}
        {@const mongo = !isSql(c)}
        <div class="relative" style="background:{guides([level])}">
          {@render status(tk, level + 1, !mongo)}
          {#if mongo && nodes[tk]?.status === 'ok'}
            {@const n = colList(tk)[0]?.sampleSize}
            {@const label = n === undefined ? 'No documents' : `Fields from a sample of ${n} document${n === 1 ? '' : 's'}`}
            <div class="flex h-[24px] items-center pr-2 text-[12px] text-faint" style={pad(level + 1, 20)} title={label}><span class="min-w-0 truncate">{label}</span></div>
          {/if}
          {#each colList(tk) as col (col.name)}
            {@const title = mongo && col.frequency !== undefined ? `${col.name} ${col.dataType} · ${Math.round(col.frequency * 100)}%` : `${col.name} ${col.dataType}${col.nullable ? '' : ' NOT NULL'}${col.default != null ? ` DEFAULT ${col.default}` : ''}`}
            <div class="flex h-[24px] items-center gap-2 pr-2 text-[12px] text-muted" style={pad(level + 1, 20)} {title}>
              {#if col.isPk}<KeyRound size={12} class="shrink-0 text-accent" />{:else}<Columns2 size={12} class="shrink-0 text-faint" />{/if}
              <!-- A long Mongo type list must not squeeze the field name to nothing. -->
              <span class="{mongo ? 'min-w-10' : 'min-w-0'} flex-1 truncate {col.isPk ? 'text-ink' : ''}">{col.name}</span>
              <span class="max-w-24 shrink-0 truncate font-mono text-[10.5px] text-faint">{col.dataType}</span>
              {#if mongo && col.frequency !== undefined}<span class="shrink-0 font-mono text-[10.5px] text-faint tabular-nums">{Math.round(col.frequency * 100)}%</span>{/if}
            </div>
          {/each}
        </div>
      {/if}
    {/each}
  </div>
{/snippet}

{#snippet tableRows(c: Conn, pk0: string, tables: string[], level: number, anc: number[])}
  <div class="relative" style="background:{guides(anc)}">
    {#each tables as t (t)}
      {@const tk = `${pk0}/${t}`}
      {@const sel = hl === `${c.id}/${nsOf(c.engine, t)}/${t}`}
      {@const ref = { database: c.database, schema: c.engine === 'pg' ? nsOf(c.engine, t) : null, name: t, kind: 'table' as const }}
      <div class="relative">
        <button class="nav-item select-none {sel ? 'nav-item-active' : ''}" style={pad(level, 20)} title={c.engine === 'pg' ? `${nsOf(c.engine, t)}.${t}` : t} onclick={(e) => tableClick(e, `${c.id}/${nsOf(c.engine, t)}/${t}`, c, ref)} ondblclick={() => onopen(c, ref)} onkeydown={(e) => tableKey(e, c, ref)} oncontextmenu={(e) => openTableMenu(e, c, ref, null)}>
          <Table2 size={13} class="shrink-0 {sel ? 'text-accent' : 'text-faint'}" />
          <span class="min-w-0 flex-1 truncate">{t}</span>
        </button>
        {@render toggleBtn(level, !!open[tk], `Toggle columns of ${t}`, () => toggle(tk))}
      </div>
      {#if open[tk]}
        <div class="relative" style="background:{guides([level])}">
          {#each schemas[c.schema][t] as col (col)}
            {@const pk = pkOf(t).includes(col)}
            <div class="flex h-[24px] items-center gap-2 pr-2 text-[12px] text-muted" style={pad(level + 1, 20)}>
              {#if pk}<KeyRound size={12} class="shrink-0 text-accent" />{:else}<Columns2 size={12} class="shrink-0 text-faint" />{/if}
              <span class="min-w-0 flex-1 truncate {pk ? 'text-ink' : ''}">{col}</span>
              <span class="shrink-0 font-mono text-[10.5px] text-faint">{colType(schemas[c.schema], t, col)}</span>
            </div>
          {/each}
        </div>
      {/if}
    {/each}
  </div>
{/snippet}

<svelte:window
  onclick={() => (menu = tableMenu = null)}
  oncontextmenu={() => (menu = tableMenu = null)}
  onkeydown={(e) => { if (e.key === 'Escape') menu = tableMenu = drag = null; }}
  onpointermove={dragMove}
  onpointerup={dragEnd}
  onpointercancel={() => (drag = null)}
/>

<div class="label flex h-7 items-center px-2">
  <span class="flex-1">Connections</span>
  <span class="mr-1.5 tabular-nums text-faint">{db.connections.length}</span>
  <button class="grid size-5 place-items-center rounded text-faint hover:bg-hover hover:text-ink" title="New connection" aria-label="New connection" onclick={onnew}><Plus size={13} /></button>
</div>

{#if db.loading}
  <p class="px-2 py-1 text-[12px] text-faint">Loading…</p>
{:else if db.loadError}
  <p class="px-2 py-1 text-[12px] text-red-700 dark:text-red-400" role="alert" title={db.loadError}>Could not load connections</p>
{/if}

{#each db.connections as c, i (c.id)}
  {@const isActive = c.id === active().id}
  {@const menuOn = menu?.c.id === c.id}
  {@const sql = isSql(c)}
  <div class="group relative {drag?.on && drag.id === c.id ? 'opacity-50' : ''}" data-conn-row>
    {#if drag?.on && drag.to === i}<div class="pointer-events-none absolute inset-x-1 -top-px z-10 h-0.5 rounded-full bg-accent" aria-hidden="true"></div>{/if}
    <!-- detail > 1: the 2nd click of a double-click must not collapse the row again. -->
    <button
      class="flex h-7 w-full items-center gap-2 rounded-md pr-2 text-left text-[13px] {isActive ? 'font-medium text-ink' : 'text-muted hover:text-ink'} {menuOn ? 'bg-active' : 'hover:bg-hover'} {drag?.on ? 'cursor-grabbing' : ''}"
      style={pad(0)}
      title="{c.name} · {c.version} · Environment: {c.env}{canWrite(c) ? '' : ' · read-only'} · Drag or Alt+↑/↓ to reorder"
      aria-expanded={!!open[c.id]}
      onclick={(e) => { if (e.detail > 1 || dragged) return; open[c.id] ? (open[c.id] = false) : expand(c); }}
      oncontextmenu={(e) => openMenu(e, c)}
      onpointerdown={(e) => dragStart(e, c)}
      onkeydown={(e) => rowKey(e, c, i)}
    >
      <ChevronRight size={12} class="shrink-0 text-faint transition-transform {open[c.id] ? 'rotate-90' : ''}" />
      <span class="w-[15px] shrink-0 font-mono text-[10px] font-semibold tracking-tight {engines[c.engine].badge}">{engines[c.engine].short}</span>
      <span class="min-w-0 flex-1 truncate">{c.name}</span>
      <span class="sr-only">{c.env}{canWrite(c) ? '' : ', read-only'}</span>
      <!-- Hidden on hover: the actions button covers that corner. The row title and the sr-only text above carry the same facts. -->
      <span class="flex shrink-0 items-center gap-2 {menuOn ? 'invisible' : 'group-focus-within:invisible group-hover:invisible'}">
        {#if !canWrite(c)}<Lock size={11} class="text-faint" aria-hidden="true" />{/if}
        {#if c.env === 'production'}<span class="tag {envTag.production}" aria-hidden="true">production</span>{:else}<span class="size-1.5 rounded-full {envDot[c.env]}" aria-hidden="true"></span>{/if}
      </span>
    </button>
    <button
      class="absolute top-1 right-1 grid size-5 place-items-center rounded text-faint hover:bg-active hover:text-ink {menuOn ? 'bg-active text-ink' : 'opacity-0 group-hover:opacity-100 focus-visible:opacity-100'}"
      title="Connection actions"
      aria-label="Actions for {c.name}"
      onclick={(e) => toggleMenuFromButton(e, c)}
    >
      <Ellipsis size={13} />
    </button>
  </div>
  {#if open[c.id] && (!isMock || !sql)}
    <div class="relative" style="background:{rootGuide(c)}">
      {@render status(c.id, 1, true)}
      {#each strings(c.id) as d (d)}
        {@const dk = `${c.id}/${d}`}
        {@const dn = nodes[dk]}
        <div class="group/n relative">
          <button class="nav-item pr-7" style={pad(1)} title={d} aria-expanded={!!open['o:' + dk]} onclick={() => (open['o:' + dk] ? (open['o:' + dk] = false) : openDb(c, d))}>
            <ChevronRight size={12} class="shrink-0 text-faint transition-transform {open['o:' + dk] ? 'rotate-90' : ''}" />
            <Database size={13} class="shrink-0 text-faint" />
            <span class="min-w-0 flex-1 truncate {d === c.database ? 'text-ink' : ''}">{d}</span>
            {#if dn?.status === 'ok'}<span class="text-[11px] text-faint tabular-nums group-hover/n:invisible">{dn.items.length}</span>{/if}
          </button>
          {@render refreshBtn(dk, d)}
        </div>
        {#if open['o:' + dk]}
          <div class="relative" style="background:{guides([1])}">
            {@render status(dk, 2, true)}
            {#if c.engine === 'pg'}
              {#each strings(dk) as s (s)}
                {@const sk = `${dk}/${s}`}
                {@const sn = nodes[sk]}
                <div class="group/n relative">
                  <button class="nav-item pr-7" style={pad(2)} aria-expanded={!!open['o:' + sk]} onclick={() => (open['o:' + sk] ? (open['o:' + sk] = false) : openSchema(c, d, s))}>
                    <ChevronRight size={12} class="shrink-0 text-faint transition-transform {open['o:' + sk] ? 'rotate-90' : ''}" />
                    <Layers size={13} class="shrink-0 text-faint" />
                    <span class="min-w-0 flex-1 truncate">{s}</span>
                    {#if sn?.status === 'ok'}<span class="text-[11px] text-faint tabular-nums group-hover/n:invisible">{visible(sn.items as TableInfo[]).length}</span>{/if}
                  </button>
                  {@render refreshBtn(sk, s)}
                </div>
                {#if open['o:' + sk]}
                  <div class="relative" style="background:{guides([2])}">{@render status(sk, 3, true)}</div>
                  {@render realTables(c, d, s, 3, [2])}
                {/if}
              {/each}
            {:else}
              {@render realTables(c, d, null, 2, [])}
            {/if}
          </div>
        {/if}
      {/each}
    </div>
  {:else if open[c.id]}
    {@const dk = `${c.id}/db`}
    {@const gs = groups(c)}
    <div class="relative" style="background:{rootGuide(c)}">
      <button class="nav-item" style={pad(1)} title={c.database} aria-expanded={!!open[dk]} onclick={() => toggle(dk)}>
        <ChevronRight size={12} class="shrink-0 text-faint transition-transform {open[dk] ? 'rotate-90' : ''}" />
        <Database size={13} class="shrink-0 text-faint" />
        <span class="min-w-0 flex-1 truncate">{c.database}</span>
        <span class="text-[11px] text-faint tabular-nums">{c.engine === 'pg' ? gs.length : Object.keys(schemas[c.schema]).length}</span>
      </button>
      {#if open[dk]}
        <div class="relative" style="background:{guides([1])}">
          {#if c.engine === 'pg'}
            {#each gs as [ns, tables] (ns)}
              {@const nk = `${dk}/${ns}`}
              <button class="nav-item" style={pad(2)} aria-expanded={!!open[nk]} onclick={() => toggle(nk)}>
                <ChevronRight size={12} class="shrink-0 text-faint transition-transform {open[nk] ? 'rotate-90' : ''}" />
                <Layers size={13} class="shrink-0 text-faint" />
                <span class="min-w-0 flex-1 truncate">{ns}</span>
                <span class="text-[11px] text-faint tabular-nums">{tables.length}</span>
              </button>
              {#if open[nk]}{@render tableRows(c, nk, tables, 3, [2])}{/if}
            {/each}
          {:else}
            {@render tableRows(c, dk, Object.keys(schemas[c.schema]), 2, [])}
          {/if}
        </div>
      {/if}
    </div>
  {/if}
{/each}
{#if drag?.on && drag.to === db.connections.length}<div class="relative" aria-hidden="true"><div class="pointer-events-none absolute inset-x-1 -top-px h-0.5 rounded-full bg-accent"></div></div>{/if}

{#if menu}
  {@const m = menu}
  <div class="popover fixed z-40 w-44 p-1" style="left:{m.x}px;top:{m.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    <div class="truncate px-2 pt-1 pb-1 text-[11.5px] text-faint">{m.c.name}</div>
    <button class="nav-item" role="menuitem" onclick={() => act(onedit)}><Pencil size={13} /> Edit connection…</button>
    <button class="nav-item" role="menuitem" onclick={() => act(onduplicate)}><Copy size={13} /> Duplicate</button>
    {#if !isMock || !isSql(m.c)}<button class="nav-item" role="menuitem" onclick={() => act((c) => refresh(c.id))}><RotateCw size={13} /> Refresh</button>{/if}
    <div class="mx-2 my-1 h-px bg-line"></div>
    <button class="nav-item text-red-600 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300" role="menuitem" onclick={() => act(ondelete)}><Trash size={13} /> Delete…</button>
  </div>
{/if}

{#if tableMenu}
  {@const m = tableMenu}
  {@const isView = m.t.kind !== 'table'}
  {@const noExport = canExport ? undefined : 'Needs the desktop app'}
  <!-- A MongoDB collection has its own menu: the script that creates it, and the document export. -->
  {@const mongo = !isSql(m.c)}
  <div class="popover fixed z-40 w-60 p-1" style="left:{m.x}px;top:{m.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    <div class="truncate px-2 pt-1 pb-1 text-[11.5px] text-faint" title={displayName(m.t)}>{displayName(m.t)}</div>
    <button class="nav-item" role="menuitem" onclick={() => tableAct('view')}><Table2 size={13} /> View data</button>
    {#if !mongo}
      <button class="nav-item" role="menuitem" onclick={() => tableAct('count')}><Hash size={13} /> Count rows</button>
    {/if}
    <button class="nav-item" role="menuitem" onclick={copyName}><Copy size={13} /> Copy name</button>
    {#if mongo}
      <div class="mx-2 my-1 h-px bg-line"></div>
      <button class="nav-item" role="menuitem" onclick={() => tableAct('ddl')}><FileCode size={13} /> Generate script</button>
      <div class="mx-2 my-1 h-px bg-line"></div>
      <button class="nav-item" role="menuitem" disabled={!canExport} title={noExport} onclick={() => tableAct('exportDdl')}><Download size={13} /> Export script...</button>
      <button class="nav-item" role="menuitem" disabled={!canExport} title={noExport} onclick={() => tableAct('exportSql')}><Download size={13} /> Export script with data...</button>
      <button class="nav-item" role="menuitem" disabled={!canExport} title={noExport} onclick={() => tableAct('exportDocuments')}><Sheet size={13} /> Export collection...</button>
    {:else}
      <div class="mx-2 my-1 h-px bg-line"></div>
      <button class="nav-item" role="menuitem" onclick={() => tableAct('ddl')}><FileCode size={13} /> Generate DDL</button>
      <button class="nav-item" role="menuitem" onclick={() => tableAct('select')}><FileSearch size={13} /> Generate SELECT</button>
      <button class="nav-item" role="menuitem" disabled={isView} onclick={() => tableAct('insert')}><FilePlus size={13} /> Generate INSERT</button>
      <div class="mx-2 my-1 h-px bg-line"></div>
      <button class="nav-item" role="menuitem" disabled={!canExport} title={noExport} onclick={() => tableAct('exportDdl')}><Download size={13} /> Export DDL...</button>
      <button class="nav-item" role="menuitem" disabled={!canExport} title={noExport} onclick={() => tableAct('exportSql')}><Download size={13} /> Export as SQL (DDL + data)...</button>
      <button class="nav-item" role="menuitem" disabled={!canExport} title={noExport} onclick={() => tableAct('exportCsv')}><Sheet size={13} /> Export as CSV...</button>
    {/if}
    {#if m.list}
      {@const list = m.list}
      <div class="mx-2 my-1 h-px bg-line"></div>
      <button class="nav-item" role="menuitem" onclick={() => { tableMenu = null; refresh(list); }}><RotateCw size={13} /> Refresh</button>
    {/if}
    {#if !mongo}
      <div class="mx-2 my-1 h-px bg-line"></div>
      <button class="nav-item text-red-600 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300" role="menuitem" disabled={isView || m.c.readOnly} title={m.c.readOnly ? 'The connection is read-only' : undefined} onclick={() => tableAct('truncate')}><Eraser size={13} /> Truncate…</button>
      <button class="nav-item text-red-600 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300" role="menuitem" disabled={m.c.readOnly} title={m.c.readOnly ? 'The connection is read-only' : undefined} onclick={() => tableAct('drop')}><Trash size={13} /> Drop {kindLabel[m.t.kind]}…</button>
    {/if}
  </div>
{/if}
