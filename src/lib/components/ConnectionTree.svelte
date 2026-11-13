<script lang="ts">
  import { ChevronRight, Table2, Plus, Layers, KeyRound, Columns2, Database, Pencil, Copy, Trash, Ellipsis, LoaderCircle, RotateCw, Eye, ScanEye, ListTree, Lock, Hash, FileCode, FileSearch, FilePlus, Download, Sheet, Eraser } from '@lucide/svelte';
  import { untrack } from 'svelte';
  import { api, type TableInfo, type TableKind } from '../api';
  import { catalog, databases, forget as forgetCatalog, listKey, loadColumns, loadDatabases, loadSchemas, loadTables, reloadLists, schemas, tables, type Loaded } from '../catalog.svelte';
  import { db, engines, envDot, envTag, canWrite, defaultDb, isSql, sameTarget, type Conn } from '../connections.svelte';
  import { active } from '../tabs.svelte';
  import { Pref } from '../persist.svelte';
  import { displayName, kindLabel, type TableAction, type TableRef } from '../tableActions';
  import { tableKey } from '../tableId';

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
  function tableKeydown(e: KeyboardEvent, c: Conn, t: TableRef) {
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
  // The click that ends a drag must not collapse the row; the next press clears the flag.
  let dragged = false;

  function dragStart(e: PointerEvent, c: Conn) {
    if (e.button !== 0 || e.metaKey || e.ctrlKey) return;
    dragged = false;
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
    dragged = true;
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

  // Table menu. `list`: the table list the table belongs to, for Refresh.
  let tableMenu = $state<{ x: number; y: number; c: Conn; t: TableRef } | null>(null);

  function openTableMenu(e: MouseEvent, c: Conn, t: TableRef) {
    e.preventDefault();
    e.stopPropagation();
    menu = null;
    const r = e.clientX === 0 && e.clientY === 0 ? (e.currentTarget as HTMLElement).getBoundingClientRect() : null;
    // Menu is w-60 (240px) and about 470px tall; keep it inside the window.
    tableMenu = { x: Math.max(4, Math.min(r ? r.left + 16 : e.clientX, innerWidth - 244)), y: Math.max(4, Math.min(r ? r.bottom + 2 : e.clientY, innerHeight - 474)), c, t };
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

  const canExport = api.capabilities.fileExport;

  // Keys: connection id, `id/db`, `id/db/schema` (pg only), `…/table`, and `…/<folder>#<kind>`. Kept for the session only.
  const open = $state<Record<string, boolean>>({});

  // The active connection starts expanded; also applies after a switch or a new connection.
  $effect(() => {
    const c = db.connections.find((x) => x.id === active().id);
    if (c && open[c.id] === undefined) untrack(() => expandReal(c));
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

  const dbLoad = (c: Conn): Loaded<string[]> | undefined => databases(c.id);
  const schemaLoad = (c: Conn, d: string): Loaded<string[]> | undefined => schemas(c.id, d);
  const tableLoad = (c: Conn, d: string, s: string | null): Loaded<TableInfo[]> | undefined => tables(c.id, d, s);
  const colLoad = (c: Conn, d: string, s: string | null, t: string) => catalog.columns[tableKey({ connId: c.id, database: d, schema: s, name: t })];

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
  const partitions = new Pref('sandb.tree.show-partitions', false, { version: 1, migrate: (v) => v === 1 || v === '1' || v === true });
  const showPartitions = $derived(partitions.value);
  const setShowPartitions = (on: boolean) => partitions.set(on);
  const visible = (list: TableInfo[]) => (showPartitions ? list : list.filter((t) => !t.partitionOf));

  /** After a structure change the catalog reloads its lists; the tree's Refresh reloads what was loaded. */
  function refresh(c: Conn) {
    void reloadLists(c.id);
  }

  /** `auto`: on pg, also open `public` (or the first schema) once the schemas are in. */
  async function openDb(c: Conn, d: string, auto = false) {
    const k = `${c.id}/${d}`;
    open[k] = true;
    if (c.engine !== 'pg') {
      await loadTables(c.id, d, null);
      return;
    }
    await loadSchemas(c.id, d);
    const list = schemas(c.id, d)?.items ?? [];
    const s = list.includes('public') ? 'public' : list[0];
    if (auto && s && open[`${k}/${s}`] === undefined) openSchema(c, d, s);
  }

  function openSchema(c: Conn, d: string, s: string) {
    open[`${c.id}/${d}/${s}`] = true;
    void loadTables(c.id, d, s);
  }

  function toggleCols(c: Conn, d: string, s: string | null, t: string) {
    const k = tableKey({ connId: c.id, database: d, schema: s, name: t });
    open[k] = !open[k];
    if (open[k]) void loadColumns(c.id, d, s, t);
  }

  // First expand loads the databases and opens the connection's own one (when it names one).
  function expandReal(c: Conn) {
    open[c.id] = true;
    void loadDatabases(c.id);
    const own = defaultDb(c);
    if (own && open[`${c.id}/${own}`] === undefined) void openDb(c, own, true);
  }

  // An edit that points a connection elsewhere makes everything loaded for it stale: the catalog
  // forgets it and the tree opens it again from scratch.
  const targets: Record<string, Conn> = {};
  $effect(() => {
    for (const c of db.connections) {
      const now = $state.snapshot(c);
      const prev = targets[c.id];
      if (prev && (!sameTarget(prev, now) || prev.database !== now.database)) {
        untrack(() => {
          const wasOpen = open[c.id];
          forgetCatalog(c);
          for (const k of Object.keys(open)) if (k === c.id || k.startsWith(c.id + '/')) delete open[k];
          for (const k of Object.keys(shown)) if (k.startsWith(c.id + '/')) delete shown[k];
          if (wasOpen) expandReal(c);
        });
      }
      targets[c.id] = now;
    }
  });
</script>

{#snippet status(load: Loaded<unknown[]> | undefined, level: number, empty: boolean)}
  {#if load?.status === 'loading' && !load.items.length}
    <div class="flex h-[26px] items-center gap-2 text-[12px] text-faint" style={pad(level, 20)}><LoaderCircle size={12} class="animate-spin" /> Loading…</div>
  {:else if load?.status === 'error'}
    <div class="flex h-[26px] items-center gap-1 pr-1 text-[12px] text-red-700 dark:text-red-400" style={pad(level, 20)} role="alert">
      <span class="min-w-0 flex-1 truncate" title={load.error}>{load.error}</span>
    </div>
  {:else if load?.status === 'ok' && !load.items.length && empty}
    <div class="flex h-[26px] items-center text-[12px] text-faint" style={pad(level, 20)}>Empty</div>
  {/if}
{/snippet}

{#snippet toggleBtn(level: number, isOpen: boolean, label: string, onclick: () => void)}
  <button class="absolute top-[5px] grid h-4 w-3 place-items-center rounded-sm text-faint hover:text-ink" style="left:{pad0 + level * 12}px" aria-label={label} aria-expanded={isOpen} {onclick}>
    <ChevronRight size={12} class="transition-transform {isOpen ? 'rotate-90' : ''}" />
  </button>
{/snippet}

{#snippet realTables(c: Conn, d: string, s: string | null, level: number, anc: number[])}
  {@const pk0 = listKey(c.id, d, s)}
  {@const all = tableLoad(c, d, s)?.items ?? []}
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
        {@const gOpen = open[gk] ?? g.kind === 'table'}
        {@const n = shown[gk] ?? PAGE}
        <div class="group/n relative">
          <button class="nav-item {toggle ? 'pr-7' : ''}" style={pad(level)} aria-expanded={gOpen} title={toggle && !showPartitions ? `${parts} partitions hidden` : undefined} onclick={() => (open[gk] = !gOpen)}>
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
          {@render tableItems(c, d, s, items.slice(0, n), level + 1)}
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

{#snippet tableItems(c: Conn, d: string, s: string | null, items: TableInfo[], level: number)}
  <div class="relative" style="background:{guides([level - 1])}">
    {#each items as t (t.name)}
      {@const tk = tableKey({ connId: c.id, database: d, schema: s, name: t.name })}
      {@const sel = hl === tk}
      {@const ref = { database: d, schema: s, name: t.name, kind: t.kind }}
      {@const Icon = kindIcon[t.kind]}
      <div class="relative">
        <button
          class="nav-item select-none {sel ? 'nav-item-active' : ''}"
          style={pad(level, 20)}
          title="{s ? s + '.' : ''}{t.name}{t.kind === 'table' ? '' : ` (${kindLabel[t.kind]})`}{t.partitionOf ? ` · partition of ${t.partitionOf}` : ''}{t.rowEstimate != null && t.rowEstimate >= 0 ? ` · ~${t.rowEstimate} rows` : ''}"
          onclick={(e) => tableClick(e, tk, c, ref)}
          ondblclick={() => onopen(c, ref)}
          onkeydown={(e) => tableKeydown(e, c, ref)}
          oncontextmenu={(e) => openTableMenu(e, c, ref)}
        >
          <Icon size={13} class="shrink-0 {sel ? 'text-accent' : 'text-faint'}" />
          <span class="min-w-0 flex-1 truncate">{t.name}</span>
        </button>
        {@render toggleBtn(level, !!open[tk], `Toggle columns of ${t.name}`, () => toggleCols(c, d, s, t.name))}
      </div>
      {#if open[tk]}
        {@const mongo = !isSql(c)}
        {@const cols = colLoad(c, d, s, t.name)}
        <div class="relative" style="background:{guides([level])}">
          {@render status(cols, level + 1, !mongo)}
          {#if mongo && cols?.status === 'ok'}
            {@const n = cols.items[0]?.sampleSize}
            {@const label = n === undefined ? 'No documents' : `Fields from a sample of ${n} document${n === 1 ? '' : 's'}`}
            <div class="flex h-[24px] items-center pr-2 text-[12px] text-faint" style={pad(level + 1, 20)} title={label}><span class="min-w-0 truncate">{label}</span></div>
          {/if}
          {#each cols?.items ?? [] as col (col.name)}
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
  <div class="group relative {drag?.on && drag.id === c.id ? 'opacity-50' : ''}" data-conn-row>
    {#if drag?.on && drag.to === i}<div class="pointer-events-none absolute inset-x-1 -top-px z-10 h-0.5 rounded-full bg-accent" aria-hidden="true"></div>{/if}
    <!-- detail > 1: the 2nd click of a double-click must not collapse the row again. -->
    <button
      class="flex h-7 w-full items-center gap-2 rounded-md pr-2 text-left text-[13px] {isActive ? 'font-medium text-ink' : 'text-muted hover:text-ink'} {menuOn ? 'bg-active' : 'hover:bg-hover'} {drag?.on ? 'cursor-grabbing' : ''}"
      style={pad(0)}
      title="{c.name} · {c.version} · Environment: {c.env}{canWrite(c) ? '' : ' · read-only'} · Drag or Alt+↑/↓ to reorder"
      aria-expanded={!!open[c.id]}
      onclick={(e) => { if (e.detail > 1 || dragged) { dragged = false; return; } open[c.id] ? (open[c.id] = false) : expandReal(c); }}
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
  {#if open[c.id]}
    <div class="relative" style="background:{rootGuide(c)}">
      {@render status(dbLoad(c), 1, true)}
      {#each dbLoad(c)?.items ?? [] as d (d)}
        {@const dk = `${c.id}/${d}`}
        {@const dn = schemaLoad(c, d)}
        <button class="nav-item" style={pad(1)} title={d} aria-expanded={!!open[dk]} onclick={() => (open[dk] ? (open[dk] = false) : openDb(c, d))}>
          <ChevronRight size={12} class="shrink-0 text-faint transition-transform {open[dk] ? 'rotate-90' : ''}" />
          <Database size={13} class="shrink-0 text-faint" />
          <span class="min-w-0 flex-1 truncate {d === c.database ? 'text-ink' : ''}">{d}</span>
          {#if dn?.status === 'ok'}<span class="text-[11px] text-faint tabular-nums">{dn.items.length}</span>{/if}
        </button>
        {#if open[dk]}
          <div class="relative" style="background:{guides([1])}">
            {#if c.engine === 'pg'}
              {@render status(dn, 2, true)}
              {#each dn?.items ?? [] as s (s)}
                {@const sk = `${dk}/${s}`}
                {@const sn = tableLoad(c, d, s)}
                <button class="nav-item" style={pad(2)} aria-expanded={!!open[sk]} onclick={() => (open[sk] ? (open[sk] = false) : openSchema(c, d, s))}>
                  <ChevronRight size={12} class="shrink-0 text-faint transition-transform {open[sk] ? 'rotate-90' : ''}" />
                  <Layers size={13} class="shrink-0 text-faint" />
                  <span class="min-w-0 flex-1 truncate">{s}</span>
                  {#if sn?.status === 'ok'}<span class="text-[11px] text-faint tabular-nums">{visible(sn.items).length}</span>{/if}
                </button>
                {#if open[sk]}
                  <div class="relative" style="background:{guides([2])}">{@render status(sn, 3, true)}</div>
                  {@render realTables(c, d, s, 3, [2])}
                {/if}
              {/each}
            {:else}
              {@render status(tableLoad(c, d, null), 2, true)}
              {@render realTables(c, d, null, 2, [])}
            {/if}
          </div>
        {/if}
      {/each}
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
    <button class="nav-item" role="menuitem" onclick={() => act((c) => refresh(c))}><RotateCw size={13} /> Refresh connection</button>
    <div class="mx-2 my-1 h-px bg-line"></div>
    <button class="nav-item text-red-600 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300" role="menuitem" onclick={() => act(ondelete)}><Trash size={13} /> Delete…</button>
  </div>
{/if}

{#if tableMenu}
  {@const m = tableMenu}
  {@const isView = m.t.kind !== 'table'}
  {@const noExport = canExport ? undefined : 'Needs the desktop app'}
  {@const readOnly = !canWrite(m.c)}
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
    <div class="mx-2 my-1 h-px bg-line"></div>
    {#if !mongo}
      <button class="nav-item text-red-600 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300" role="menuitem" disabled={isView || readOnly} title={readOnly ? 'The connection is read-only' : undefined} onclick={() => tableAct('truncate')}><Eraser size={13} /> Truncate…</button>
      <button class="nav-item text-red-600 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300" role="menuitem" disabled={readOnly} title={readOnly ? 'The connection is read-only' : undefined} onclick={() => tableAct('drop')}><Trash size={13} /> Drop {kindLabel[m.t.kind]}…</button>
    {/if}
  </div>
{/if}
