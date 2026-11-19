<script lang="ts">
  import EnvironmentBadge from './EnvironmentBadge.svelte';
  import { ChevronRight, Table2, Plus, Layers, KeyRound, Columns2, Database, Pencil, Copy, Trash, Ellipsis, LoaderCircle, RotateCw, Eye, ScanEye, ListTree, Lock, Hash, FileCode, FileSearch, FilePlus, Download, Sheet, Eraser, Clock } from '@lucide/svelte';
  import { onDestroy, untrack } from 'svelte';
  import { api, type TableInfo, type TableKind } from '../api';
  import { catalog, databases, forget as forgetCatalog, listKey, loadColumns, loadDatabases, loadRedisKeys, loadSchemas, loadTables, redisKeys, reloadLists, schemas, tables, type Loaded } from '../catalog.svelte';
  import { db, environmentLabel, canWrite, defaultDb, isMongo, isRedis, isSql, sameTarget, type Conn } from '../connections.svelte';
  import { active, activeTab, tabs } from '../tabs.svelte';
  import { Pref } from '../persist.svelte';
  import { displayName, kindLabel, type TableAction, type TableRef } from '../tableActions';
  import { tableKey } from '../tableId';
  import EngineBadge from './ui/EngineBadge.svelte';
  import Menu from './ui/Menu.svelte';

  let {
    selected,
    tableBusy,
    onopen,
    onnew,
    onedit,
    onduplicate,
    ondelete,
    ontable,
    onredisnew,
    onreorder,
  }: {
    selected: string | null;
    tableBusy?: Record<string, boolean>;
    /** Double-click, ⌘/Ctrl-click or Enter opens data. A single click selects. */
    onopen: (c: Conn, t: TableRef) => void;
    onnew: () => void;
    onedit: (c: Conn) => void;
    onduplicate: (c: Conn) => void;
    ondelete: (c: Conn) => void;
    ontable: (a: TableAction, c: Conn, t: TableRef) => void;
    onredisnew: (c: Conn, database: string) => void;
    /** A connection row was dragged (or moved with Alt+↑/↓) to a new place. `ids`: the full new order. */
    onreorder: (ids: string[]) => void;
  } = $props();

  let picked = $state<string | null>(null);
  const activeTable = $derived.by(() => {
    const tab = activeTab();
    return tab?.table ? tableKey({ connId: tab.connId, ...tab.table }) : selected;
  });
  $effect(() => {
    // The result-derived key can be the same (or null) across different tabs.
    tabs.activeId;
    activeTable;
    picked = null;
  });
  const hl = $derived(picked ?? activeTable);

  function tableClick(e: MouseEvent, key: string, c: Conn, t: TableRef) {
    tableMenu = null;
    // ⌘/Ctrl-click opens the same view as double-click.
    if (e.metaKey || e.ctrlKey) openTable(c, t);
    else picked = key;
  }
  function openTable(c: Conn, t: TableRef) {
    picked = null;
    onopen(c, t);
  }

  function tableKeydown(e: KeyboardEvent & { currentTarget: HTMLButtonElement }, key: string, c: Conn, t: TableRef) {
    if (e.key === 'Enter') {
      e.preventDefault();
      openTable(c, t);
    } else if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
      openTableMenu(e, key);
    }
  }

  type MenuTrigger = (MouseEvent | KeyboardEvent) & { currentTarget: HTMLButtonElement };
  let menu = $state<{ anchor: HTMLElement; trigger: HTMLButtonElement; c: Conn } | null>(null);

  function openMenu(e: MenuTrigger, c: Conn) {
    e.preventDefault();
    e.stopPropagation();
    showConnectionMenu(e.currentTarget, c);
  }

  function toggleMenuFromButton(e: MouseEvent & { currentTarget: HTMLButtonElement }, c: Conn) {
    if (menu?.c.id === c.id) menu = null;
    else showConnectionMenu(e.currentTarget, c);
  }

  function showConnectionMenu(trigger: HTMLButtonElement, c: Conn) {
    tableMenu = null;
    if (trigger.parentElement) menu = { anchor: trigger.parentElement, trigger, c };
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

  function rowKey(e: KeyboardEvent & { currentTarget: HTMLButtonElement }, c: Conn, i: number) {
    if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
      openMenu(e, c);
      return;
    }
    if (!e.altKey || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
    e.preventDefault();
    move(c.id, e.key === 'ArrowUp' ? i - 1 : i + 2);
  }

  let tableMenu = $state<{ anchor: HTMLButtonElement; key: string } | null>(null);

  function openTableMenu(e: MenuTrigger, key: string) {
    e.preventDefault();
    e.stopPropagation();
    menu = null;
    tableMenu = { anchor: e.currentTarget, key };
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
  const pad0 = 8;
  /** `extra`: rows without an inline chevron (tables, columns) add the chevron and gap width. */
  const pad = (level: number, extra = 0) => `padding-left:${pad0 + level * 12 + extra}px`;
  // Indent-guide x, through the chevron center of the row at `level`.
  const guide = (level: number) => pad0 + level * 12 + 5.5;
  /** One 1px vertical guide per open ancestor level, like VS Code's tree. */
  const guides = (levels: number[]) => levels.map((l) => `linear-gradient(var(--c-hover),var(--c-hover)) ${guide(l)}px 0/1px 100% no-repeat`).join(',');
  /** The connection's own guide: a 2px danger rail on production, visible under every descendant. */
  const rootGuide = (c: Conn) => (c.env === 'production' ? `linear-gradient(var(--c-danger),var(--c-danger)) ${guide(0) - 0.5}px 0/2px 100% no-repeat` : guides([0]));

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

  const keyPatterns = $state<Record<string, string>>({});
  const keyTimers: Record<string, ReturnType<typeof setTimeout>> = {};

  function cancelSearch(k: string) {
    clearTimeout(keyTimers[k]);
    delete keyTimers[k];
  }
  onDestroy(() => Object.keys(keyTimers).forEach(cancelSearch));

  function searchKeys(c: Conn, d: string, force = false) {
    const k = listKey(c.id, d, null);
    cancelSearch(k);
    void loadRedisKeys(c.id, d, keyPatterns[k] || '*', force);
  }

  function debounceSearch(c: Conn, d: string, pattern: string) {
    const k = listKey(c.id, d, null);
    keyPatterns[k] = pattern;
    cancelSearch(k);
    keyTimers[k] = setTimeout(() => {
      if (db.connections.some((conn) => conn.id === c.id && isRedis(conn))) searchKeys(c, d);
      else cancelSearch(k);
    }, 300);
  }

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
    if (isRedis(c)) {
      if (open[`${k}#keys`] !== false) searchKeys(c, d);
      return;
    }
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
          for (const k of Object.keys(keyPatterns)) if (k.startsWith(c.id + '/')) {
            cancelSearch(k);
            delete keyPatterns[k];
          }
          if (wasOpen) expandReal(c);
        });
      }
      targets[c.id] = now;
    }
  });
</script>

{#snippet status(load: Loaded<unknown[]> | undefined, level: number, empty: boolean)}
  {#if load?.status === 'loading' && !load.items.length}
    <div class="flex h-list-row items-center gap-2 text-sm text-faint" style={pad(level, 20)}><LoaderCircle size={13} class="motion-safe:animate-spin" aria-hidden="true" /> Loading…</div>
  {:else if load?.status === 'error'}
    <div class="flex h-list-row items-center gap-1 pr-1 text-sm text-danger" style={pad(level, 20)} role="alert">
      <span class="min-w-0 flex-1 truncate" title={load.error}>{load.error}</span>
    </div>
  {:else if load?.status === 'ok' && !load.items.length && empty}
    <div class="flex h-list-row items-center text-sm text-faint" style={pad(level, 20)}>Empty</div>
  {/if}
{/snippet}

{#snippet toggleBtn(level: number, isOpen: boolean, label: string, onclick: () => void)}
  <button class="focus-ring absolute top-1/2 grid h-4 w-3 -translate-y-1/2 place-items-center rounded-sm text-faint hover:text-ink" style="left:{pad0 + level * 12}px" aria-label={label} aria-expanded={isOpen} {onclick}>
    <ChevronRight size={12} class="motion-safe:transition-transform {isOpen ? 'rotate-90' : ''}" />
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
      {@const gLabel = g.kind === 'table' && isMongo(c) ? 'Collections' : g.label}
      <!-- Partitions are tables, so the toggle lives on the Tables folder. It stays when every table is a hidden partition. -->
      {@const toggle = g.kind === 'table' && parts > 0}
      {#if items.length || toggle}
        {@const gk = `${pk0}#${g.kind}`}
        <!-- The Tables folder starts open; the others start closed. -->
        {@const gOpen = open[gk] ?? g.kind === 'table'}
        {@const n = shown[gk] ?? PAGE}
        <div class="group/n relative">
          <button class="nav-item {toggle ? 'pr-7' : ''}" style={pad(level)} aria-expanded={gOpen} title={toggle && !showPartitions ? `${parts} partitions hidden` : undefined} onclick={() => (open[gk] = !gOpen)}>
            <ChevronRight size={12} class="shrink-0 text-faint motion-safe:transition-transform {gOpen ? 'rotate-90' : ''}" />
            <g.icon size={13} class="shrink-0 text-faint" />
            <span class="min-w-0 flex-1 truncate">{gLabel}</span>
            <span class="text-xs text-faint tabular-nums {toggle ? 'group-hover/n:invisible' : ''}">{items.length}</span>
          </button>
          {#if toggle}
            {@const label = showPartitions ? 'Hide partitions' : `Show ${parts} partitions`}
            <button
              class="focus-ring absolute top-1/2 right-1 grid size-5 -translate-y-1/2 place-items-center rounded opacity-0 group-hover/n:opacity-100 focus-visible:opacity-100 hover:bg-hover hover:text-ink {showPartitions ? 'text-accent' : 'text-faint'}"
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
                <span class="text-xs tabular-nums">{n} of {items.length}</span>
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

{#snippet redisFolder(c: Conn, d: string, level: number)}
  {@const k = listKey(c.id, d, null)}
  {@const folder = `${k}#keys`}
  {@const expanded = open[folder] ?? true}
  {@const load = redisKeys(c.id, d)}
  {@const items = load?.items.keys ?? []}
  <button class="nav-item" style={pad(level)} aria-expanded={expanded} onclick={() => { open[folder] = !expanded; if (!expanded) searchKeys(c, d); }}>
    <ChevronRight size={12} class="shrink-0 text-faint motion-safe:transition-transform {expanded ? 'rotate-90' : ''}" />
    <KeyRound size={13} class="shrink-0 text-faint" />
    <span class="min-w-0 flex-1 truncate">Keys</span>
    {#if load?.status === 'ok'}<span class="text-xs text-faint tabular-nums">{items.length}</span>{/if}
  </button>
  {#if expanded}
    <div style="background:{guides([level])}">
      <div class="py-1 pr-2" style={pad(level + 1)}>
        <input class="input input-sm w-full font-mono" type="text" placeholder="*" value={keyPatterns[k] ?? '*'} aria-label="Key pattern for db{d}" title="Redis glob pattern, for example user:*" autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false" oninput={(e) => debounceSearch(c, d, e.currentTarget.value)} onkeydown={(e) => {
          if (e.key === 'Enter' && !e.isComposing) { e.preventDefault(); searchKeys(c, d, true); }
        }} />
      </div>
      {@render status(load && { ...load, items }, level + 1, true)}
      {#if load?.items.truncated}
        <div class="py-1 pr-2 text-xs text-faint" style={pad(level + 1)}>First {items.length} keys. Refine the pattern.</div>
      {/if}
    </div>
    {@render tableItems(c, d, null, items, level + 1)}
  {/if}
{/snippet}

{#snippet sampleLabel(label: string, level: number)}
  <div class="flex h-list-row items-center pr-2 text-sm text-faint" style={pad(level, 20)} title={label}><span class="min-w-0 truncate">{label}</span></div>
{/snippet}

{#snippet tableItems(c: Conn, d: string, s: string | null, items: TableInfo[], level: number)}
  {@const redis = isRedis(c)}
  <div class="relative" style="background:{guides([level - 1])}">
    {#each items as t (t.name)}
      {@const tk = tableKey({ connId: c.id, database: d, schema: s, name: t.name })}
      {@const sel = hl === tk}
      {@const ref = { database: d, schema: s, name: t.name, kind: t.kind, redisType: t.redisType, rowEstimate: t.rowEstimate }}
      {@const Icon = redis ? KeyRound : kindIcon[t.kind]}
      {@const detail = redis && t.redisType ? `${t.redisType}${t.rowEstimate == null ? '' : ` · ${t.rowEstimate}${t.redisType === 'string' ? ' B' : ''}`}` : ''}
      {@const busy = tableBusy?.[tk] ?? false}
      {@const menuOpen = tableMenu?.key === tk}
      <div class="relative">
        <button
          class="nav-item select-none {sel || menuOpen ? 'nav-item-active' : ''}"
          style={pad(level, 20)}
          title={redis ? `${t.name}${detail ? ` · ${detail}` : ''}` : `${s ? s + '.' : ''}${t.name}${t.kind === 'table' ? '' : ` (${kindLabel[t.kind]})`}${t.partitionOf ? ` · partition of ${t.partitionOf}` : ''}${t.rowEstimate != null && t.rowEstimate >= 0 ? ` · ~${t.rowEstimate} rows` : ''}`}
          aria-busy={busy}
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          onclick={(e) => tableClick(e, tk, c, ref)}
          ondblclick={() => openTable(c, ref)}
          onkeydown={(e) => tableKeydown(e, tk, c, ref)}
          oncontextmenu={(e) => openTableMenu(e, tk)}
        >
          {#if busy}
            <LoaderCircle size={13} class="shrink-0 text-accent motion-safe:animate-spin" aria-hidden="true" />
          {:else}
            <Icon size={13} class="shrink-0 {sel ? 'text-accent' : 'text-faint'}" />
          {/if}
          <span class="min-w-0 flex-1 truncate">{t.name}</span>
          {#if detail}<span class="tag max-w-28 shrink-0 text-faint" title={detail}><span class="truncate">{detail}</span></span>{/if}
        </button>
        {@render toggleBtn(level, !!open[tk], `Toggle ${redis ? 'fields' : 'columns'} of ${t.name}`, () => toggleCols(c, d, s, t.name))}
      </div>
      {#if menuOpen && tableMenu}
        <Menu anchor={tableMenu.anchor} label={`Actions for ${displayName(ref)}`} class="w-60" onclose={() => (tableMenu = null)}>
          {@render tableActions(c, ref)}
        </Menu>
      {/if}
      {#if open[tk]}
        {@const mongo = isMongo(c)}
        {@const cols = colLoad(c, d, s, t.name)}
        {@const sampleStart = redis ? cols?.items.findIndex((col) => col.dataType === 'hash field') : undefined}
        <div class="relative" style="background:{guides([level])}">
          {@render status(cols, level + 1, !mongo)}
          {#if mongo && cols?.status === 'ok'}
            {@const n = cols.items[0]?.sampleSize}
            {@const label = n === undefined ? 'No documents' : `Fields from a sample of ${n} document${n === 1 ? '' : 's'}`}
            {@render sampleLabel(label, level + 1)}
          {/if}
          {#each cols?.items ?? [] as col, i (redis ? i : col.name)}
            {@const title = redis ? `${col.name} ${col.dataType}` : mongo && col.frequency !== undefined ? `${col.name} ${col.dataType} · ${Math.round(col.frequency * 100)}%` : `${col.name} ${col.dataType}${col.nullable ? '' : ' NOT NULL'}${col.default != null ? ` DEFAULT ${col.default}` : ''}`}
            {#if i === sampleStart && col.sampleSize !== undefined}
              {@render sampleLabel(`Fields from a scan of ${col.sampleSize}`, level + 1)}
            {/if}
            <div class="flex h-list-row items-center gap-2 pr-2 text-sm text-muted" style={pad(level + 1, 20)} {title}>
              {#if col.isPk}<KeyRound size={13} class="shrink-0 text-accent" />{:else}<Columns2 size={13} class="shrink-0 text-faint" />{/if}
              <!-- A long Mongo type list must not squeeze the field name to nothing. -->
              <span class="{mongo ? 'min-w-10' : 'min-w-0'} flex-1 truncate {col.isPk ? 'text-ink' : ''}">{col.name}</span>
              <span class="max-w-24 shrink-0 truncate font-mono text-2xs text-faint">{col.dataType}</span>
              {#if mongo && col.frequency !== undefined}<span class="shrink-0 font-mono text-2xs text-faint tabular-nums">{Math.round(col.frequency * 100)}%</span>{/if}
            </div>
          {/each}
        </div>
      {/if}
    {/each}
  </div>
{/snippet}

<svelte:window
  onkeydown={(e) => { if (e.key === 'Escape') drag = null; }}
  onpointermove={dragMove}
  onpointerup={dragEnd}
  onpointercancel={() => (drag = null)}
/>

<div class="label flex h-7 items-center px-2">
  <span class="flex-1">Connections</span>
  <span class="mr-1.5 tabular-nums text-faint">{db.connections.length}</span>
  <button class="focus-ring grid size-5 place-items-center rounded text-faint hover:bg-hover hover:text-ink" title="New connection" aria-label="New connection" onclick={onnew}><Plus size={13} /></button>
</div>

{#if db.loading}
  <p class="flex h-list-row items-center px-2 text-sm text-faint">Loading…</p>
{:else if db.loadError}
  <p class="flex h-list-row items-center px-2 text-sm text-danger" role="alert" title={db.loadError}>Could not load connections</p>
{/if}

{#each db.connections as c, i (c.id)}
  {@const isActive = c.id === active().id}
  {@const menuOn = menu?.c.id === c.id}
  <div class="group relative {drag?.on && drag.id === c.id ? 'opacity-50' : ''}" data-conn-row>
    {#if drag?.on && drag.to === i}<div class="pointer-events-none absolute inset-x-1 -top-px z-sticky h-0.5 rounded-full bg-accent" aria-hidden="true"></div>{/if}
    <!-- detail > 1: the 2nd click of a double-click must not collapse the row again. -->
    <button
      class="nav-item {isActive ? 'font-medium text-ink' : ''} {menuOn ? 'bg-active hover:bg-active' : ''} {drag?.on ? 'cursor-grabbing' : ''}"
      style={pad(0)}
      title="{c.name} · {c.version} · Environment: {environmentLabel(c)}{canWrite(c) ? '' : ' · read-only'} · Drag or Alt+↑/↓ to reorder"
      aria-expanded={!!open[c.id]}
      onclick={(e) => { menu = null; if (e.detail > 1 || dragged) { dragged = false; return; } open[c.id] ? (open[c.id] = false) : expandReal(c); }}
      oncontextmenu={(e) => openMenu(e, c)}
      onpointerdown={(e) => dragStart(e, c)}
      onkeydown={(e) => rowKey(e, c, i)}
    >
      <ChevronRight size={12} class="shrink-0 text-faint motion-safe:transition-transform {open[c.id] ? 'rotate-90' : ''}" />
      <EngineBadge engine={c.engine} />
      <span class="min-w-0 flex-1 truncate">{c.name}</span>
      <span class="sr-only">{environmentLabel(c)}{c.env === 'production' ? ', protected writes' : ''}{canWrite(c) ? '' : ', read-only'}</span>
      <!-- Hidden on hover: the actions button covers that corner. The row title and the sr-only text above carry the same facts. -->
      <span aria-hidden="true" class="flex shrink-0 items-center gap-2 {menuOn ? 'invisible' : 'group-focus-within:invisible group-hover:invisible'}">
        {#if !canWrite(c)}<Lock size={12} class="text-faint" aria-hidden="true" />{/if}
        <EnvironmentBadge conn={c} compact />
      </span>
    </button>
    <button
      class="focus-ring absolute top-1/2 right-1 grid size-5 -translate-y-1/2 place-items-center rounded text-faint hover:bg-active hover:text-ink {menuOn ? 'bg-active text-ink' : 'opacity-0 group-hover:opacity-100 focus-visible:opacity-100'}"
      title="Connection actions"
      aria-label="Actions for {c.name}"
      aria-haspopup="menu"
      aria-expanded={menuOn}
      onclick={(e) => toggleMenuFromButton(e, c)}
    >
      <Ellipsis size={13} />
    </button>
  </div>
  {#if menuOn && menu}
    <Menu anchor={menu.anchor} returnFocus={menu.trigger} label={`Actions for ${c.name}`} onclose={() => (menu = null)}>
      <div class="truncate px-2 py-1 text-xs text-faint">{c.name}</div>
      <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => onedit(c)}><Pencil size={13} /> Edit connection…</button>
      <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => onduplicate(c)}><Copy size={13} /> Duplicate</button>
      <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => refresh(c)}><RotateCw size={13} /> Refresh connection</button>
      <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
      <button type="button" class="menu-item nav-item-danger" role="menuitem" tabindex="-1" onclick={() => ondelete(c)}><Trash size={13} /> Delete…</button>
    </Menu>
  {/if}
  {#if open[c.id]}
    <div class="relative" style="background:{rootGuide(c)}">
      {@render status(dbLoad(c), 1, true)}
      {#each dbLoad(c)?.items ?? [] as d (d)}
        {@const dk = `${c.id}/${d}`}
        {@const dn = schemaLoad(c, d)}
        {@const redis = isRedis(c)}
        {@const dbName = redis ? `db${d}` : d}
        {@const dbMenu = redis && tableMenu?.key === dk}
        <div class="group/db relative">
          <button class="nav-item {dbMenu ? 'nav-item-active' : ''}" style={pad(1)} title={dbName} aria-expanded={!!open[dk]} onclick={() => { tableMenu = null; open[dk] ? (open[dk] = false) : openDb(c, d); }} oncontextmenu={(e) => { if (redis) openTableMenu(e, dk); }} onkeydown={(e) => { if (redis && (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10'))) openTableMenu(e, dk); }}>
            <ChevronRight size={12} class="shrink-0 text-faint motion-safe:transition-transform {open[dk] ? 'rotate-90' : ''}" />
            <Database size={13} class="shrink-0 text-faint" />
            <span class="min-w-0 flex-1 truncate {d === c.database ? 'text-ink' : ''}">{dbName}</span>
            {#if dn?.status === 'ok'}<span class="text-xs text-faint tabular-nums">{dn.items.length}</span>{/if}
          </button>
          {#if redis}
            <button class="focus-ring absolute top-1/2 right-1 grid size-5 -translate-y-1/2 place-items-center rounded text-faint hover:bg-active hover:text-ink {dbMenu ? 'bg-active text-ink' : 'opacity-0 group-hover/db:opacity-100 focus-visible:opacity-100'}" title="Database actions" aria-label="Actions for {dbName}" aria-haspopup="menu" aria-expanded={dbMenu} onclick={(e) => { if (dbMenu) tableMenu = null; else openTableMenu(e, dk); }}>
              <Ellipsis size={13} />
            </button>
          {/if}
        </div>
        {#if dbMenu && tableMenu}
          <Menu anchor={tableMenu.anchor} label={`Actions for ${dbName}`} onclose={() => (tableMenu = null)}>
            <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={!canWrite(c)} title={canWrite(c) ? undefined : 'The connection is read-only'} onclick={() => onredisnew(c, d)}><Plus size={13} /> New key…</button>
            <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => searchKeys(c, d, true)}><RotateCw size={13} /> Refresh</button>
          </Menu>
        {/if}
        {#if open[dk]}
          <div class="relative" style="background:{guides([1])}">
            {#if c.engine === 'pg'}
              {@render status(dn, 2, true)}
              {#each dn?.items ?? [] as s (s)}
                {@const sk = `${dk}/${s}`}
                {@const sn = tableLoad(c, d, s)}
                <button class="nav-item" style={pad(2)} aria-expanded={!!open[sk]} onclick={() => (open[sk] ? (open[sk] = false) : openSchema(c, d, s))}>
                  <ChevronRight size={12} class="shrink-0 text-faint motion-safe:transition-transform {open[sk] ? 'rotate-90' : ''}" />
                  <Layers size={13} class="shrink-0 text-faint" />
                  <span class="min-w-0 flex-1 truncate">{s}</span>
                  {#if sn?.status === 'ok'}<span class="text-xs text-faint tabular-nums">{visible(sn.items).length}</span>{/if}
                </button>
                {#if open[sk]}
                  <div class="relative" style="background:{guides([2])}">{@render status(sn, 3, true)}</div>
                  {@render realTables(c, d, s, 3, [2])}
                {/if}
              {/each}
            {:else if redis}
              {@render redisFolder(c, d, 2)}
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

{#snippet tableActions(c: Conn, t: TableRef)}
  {@const isView = t.kind !== 'table'}
  {@const noExport = canExport ? undefined : 'Needs the desktop app'}
  {@const readOnly = !canWrite(c)}
  <!-- A MongoDB collection has its own menu: the script that creates it, and the document export. -->
  {@const mongo = isMongo(c)}
  {@const redis = isRedis(c)}
  <div class="truncate px-2 py-1 text-xs text-faint" title={displayName(t)}>{displayName(t)}</div>
  <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => { picked = null; ontable('view', c, t); }}><Table2 size={13} /> View data</button>
  {#if isSql(c)}
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => ontable('count', c, t)}><Hash size={13} /> Count rows</button>
  {/if}
  <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => navigator.clipboard?.writeText(displayName(t)).catch(() => {})}><Copy size={13} /> Copy name</button>
  {#if redis}
    {@const locked = readOnly ? 'The connection is read-only' : undefined}
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => { picked = null; ontable('properties', c, t); }}><ListTree size={13} /> Properties</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => ontable('ddl', c, t)}><FileCode size={13} /> Generate script</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={!canExport} title={noExport} onclick={() => ontable('exportDdl', c, t)}><Download size={13} /> Export script…</button>
    <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={readOnly} title={locked} onclick={() => ontable('renameKey', c, t)}><Pencil size={13} /> Rename…</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={readOnly} title={locked} onclick={() => ontable('ttlKey', c, t)}><Clock size={13} /> Set TTL…</button>
    <button type="button" class="menu-item nav-item-danger" role="menuitem" tabindex="-1" disabled={readOnly} title={locked} onclick={() => ontable('deleteKey', c, t)}><Trash size={13} /> Delete key…</button>
  {:else if mongo}
    <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => ontable('ddl', c, t)}><FileCode size={13} /> Generate script</button>
    <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={!canExport} title={noExport} onclick={() => ontable('exportDdl', c, t)}><Download size={13} /> Export script...</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={!canExport} title={noExport} onclick={() => ontable('exportSql', c, t)}><Download size={13} /> Export script with data...</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={!canExport} title={noExport} onclick={() => ontable('exportDocuments', c, t)}><Sheet size={13} /> Export collection...</button>
  {:else if isSql(c)}
    <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => ontable('ddl', c, t)}><FileCode size={13} /> Generate DDL</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => ontable('select', c, t)}><FileSearch size={13} /> Generate SELECT</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={isView} onclick={() => ontable('insert', c, t)}><FilePlus size={13} /> Generate INSERT</button>
    <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={!canExport} title={noExport} onclick={() => ontable('exportDdl', c, t)}><Download size={13} /> Export DDL...</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={!canExport} title={noExport} onclick={() => ontable('exportSql', c, t)}><Download size={13} /> Export as SQL (DDL + data)...</button>
    <button type="button" class="menu-item" role="menuitem" tabindex="-1" disabled={!canExport} title={noExport} onclick={() => ontable('exportCsv', c, t)}><Sheet size={13} /> Export as CSV...</button>
    <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
    <button type="button" class="menu-item nav-item-danger" role="menuitem" tabindex="-1" disabled={isView || readOnly} title={readOnly ? 'The connection is read-only' : undefined} onclick={() => ontable('truncate', c, t)}><Eraser size={13} /> Truncate…</button>
    <button type="button" class="menu-item nav-item-danger" role="menuitem" tabindex="-1" disabled={readOnly} title={readOnly ? 'The connection is read-only' : undefined} onclick={() => ontable('drop', c, t)}><Trash size={13} /> Drop {kindLabel[t.kind]}…</button>
  {/if}
{/snippet}
