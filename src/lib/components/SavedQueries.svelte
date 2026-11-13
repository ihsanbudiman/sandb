<script lang="ts">
  import { FileCode, Search, Ellipsis, Play, Pencil, Copy, CopyPlus, Trash, BookmarkPlus, X } from '@lucide/svelte';
  import { focusOn, selectOnMount } from '../actions';
  import { ago } from '../chatParts';
  import { db, engines, type Conn } from '../connections.svelte';
  import { active } from '../tabs.svelte';
  import { tabs } from '../tabs.svelte';
  import { KEYS } from '../keys';
  import { saved, isDirty, renameSaved, duplicateSaved, type SavedQuery } from '../saved.svelte';

  let {
    activeId,
    onopen,
    onrun,
    ondelete,
  }: {
    /** The saved query of the active tab, highlighted in the list. */
    activeId: string | null;
    onopen: (q: SavedQuery) => void;
    onrun: (q: SavedQuery) => void;
    ondelete: (q: SavedQuery) => void;
  } = $props();

  let q = $state('');
  let listEl = $state<HTMLDivElement>();
  let renaming = $state<string | null>(null);
  let menu = $state<{ x: number; y: number; q: SavedQuery } | null>(null);

  const byName = (a: SavedQuery, b: SavedQuery) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });

  // One group per connection, the active connection first; queries of a deleted connection go last.
  const groups = $derived.by(() => {
    const needle = q.trim().toLowerCase();
    const hits = needle ? saved.list.filter((s) => s.name.toLowerCase().includes(needle) || s.sql.toLowerCase().includes(needle)) : saved.list;
    const cur = active().id;
    const conns = [...db.connections].sort((a, b) => Number(b.id === cur) - Number(a.id === cur));
    const out: { key: string; conn?: Conn; items: SavedQuery[] }[] = conns
      .map((c) => ({ key: c.id, conn: c, items: hits.filter((s) => s.connId === c.id).sort(byName) }))
      .filter((g) => g.items.length);
    const orphans = hits.filter((s) => !db.connections.some((c) => c.id === s.connId)).sort(byName);
    if (orphans.length) out.push({ key: '', items: orphans });
    return out;
  });
  const shown = $derived(groups.reduce((n, g) => n + g.items.length, 0));

  /** Linked tabs with unsaved text, by saved query id. */
  const dirty = $derived(new Set(tabs.list.filter(isDirty).map((t) => t.savedId)));

  const preview = (sql: string) => sql.replace(/--.*$/gm, '').replace(/\s+/g, ' ').trim();

  // ---- menu ----
  const place = (x: number, y: number, s: SavedQuery) => ({ x: Math.max(4, Math.min(x, innerWidth - 212)), y: Math.max(4, Math.min(y, innerHeight - 220)), q: s });

  function openMenu(e: MouseEvent, s: SavedQuery) {
    e.preventDefault();
    e.stopPropagation();
    const r = e.clientX === 0 && e.clientY === 0 ? (e.currentTarget as HTMLElement).getBoundingClientRect() : null;
    menu = place(r ? r.left + 16 : e.clientX, r ? r.bottom + 2 : e.clientY, s);
  }

  function toggleMenu(e: MouseEvent & { currentTarget: HTMLElement }, s: SavedQuery) {
    e.stopPropagation();
    if (menu?.q.id === s.id) return (menu = null);
    const r = e.currentTarget.getBoundingClientRect();
    menu = place(r.left, r.bottom + 2, s);
  }

  function act(fn: (s: SavedQuery) => void) {
    const s = menu?.q;
    menu = null;
    if (s) fn(s);
  }

  // Which row takes focus back after its rename ends; the counter makes every request a new token.
  let focusRequest = $state<{ id: string; n: number } | null>(null);
  let focusSeq = 0;

  function startRename(s: SavedQuery) {
    renaming = s.id;
  }

  function finishRename(s: SavedQuery, name: string) {
    if (renaming !== s.id) return;
    renaming = null;
    renameSaved(s.id, name);
    focusRequest = { id: s.id, n: ++focusSeq };
  }

  function cancelRename(s: SavedQuery) {
    renaming = null;
    focusRequest = { id: s.id, n: ++focusSeq };
  }

  async function duplicate(s: SavedQuery) {
    const copy = duplicateSaved(s.id);
    if (copy) startRename(copy);
  }

  // Arrow keys walk the rows; F2 renames; Delete removes; Enter (a button click) opens; ⌘↵ runs.
  function rowKey(e: KeyboardEvent, s: SavedQuery) {
    const rows = [...(listEl?.querySelectorAll<HTMLButtonElement>('[data-saved]') ?? [])];
    const i = rows.indexOf(e.currentTarget as HTMLButtonElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const next = rows[i + (e.key === 'ArrowDown' ? 1 : -1)];
      if (next) next.focus();
      else if (e.key === 'ArrowUp') filterEl?.focus();
    } else if (e.key === 'F2') {
      e.preventDefault();
      startRename(s);
    } else if (e.key === 'Delete' || (e.key === 'Backspace' && (e.metaKey || e.ctrlKey))) {
      e.preventDefault();
      (rows[i + 1] ?? rows[i - 1])?.focus();
      ondelete(s);
    } else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      onrun(s);
    }
  }

  let filterEl = $state<HTMLInputElement>();
  function filterKey(e: KeyboardEvent) {
    const first = listEl?.querySelector<HTMLButtonElement>('[data-saved]');
    if (e.key === 'ArrowDown' && first) {
      e.preventDefault();
      first.focus();
    } else if (e.key === 'Enter' && groups[0]) {
      e.preventDefault();
      onopen(groups[0].items[0]);
    } else if (e.key === 'Escape' && q) {
      e.stopPropagation();
      q = '';
    }
  }
</script>

<svelte:window onclick={() => (menu = null)} oncontextmenu={() => (menu = null)} onkeydown={(e) => { if (e.key === 'Escape') menu = null; }} />

<div class="label flex h-7 items-center px-2">
  <span class="flex-1">Saved queries</span>
  <span class="tabular-nums text-faint">{saved.list.length}</span>
</div>

{#if saved.list.length}
  <div class="relative mb-1.5 px-0.5">
    <Search size={12} class="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-faint" />
    <input
      bind:this={filterEl}
      bind:value={q}
      onkeydown={filterKey}
      class="h-7 w-full rounded-md border border-line bg-field pr-6 pl-7 text-[12.5px] text-ink outline-none placeholder:text-faint focus:border-accent/70 focus:ring-3 focus:ring-accent/15"
      placeholder="Filter by name or SQL"
      aria-label="Filter saved queries"
      autocapitalize="off"
      autocorrect="off"
      spellcheck="false"
      autocomplete="off"
    />
    {#if q}
      <button class="absolute top-1/2 right-1.5 grid size-4 -translate-y-1/2 place-items-center rounded text-faint hover:text-ink" aria-label="Clear filter" onclick={() => { q = ''; filterEl?.focus(); }}><X size={11} /></button>
    {/if}
  </div>
{/if}

<div bind:this={listEl} role="list">
  {#each groups as g (g.key)}
    <div class="flex h-6 items-center gap-1.5 px-2 pt-1 text-[11.5px] text-faint" role="presentation">
      {#if g.conn}
        <span class="font-mono text-[9.5px] font-semibold {engines[g.conn.engine].badge}">{engines[g.conn.engine].short}</span>
        <span class="min-w-0 flex-1 truncate">{g.conn.name}</span>
      {:else}
        <span class="min-w-0 flex-1 truncate">Deleted connection</span>
      {/if}
      <span class="tabular-nums">{g.items.length}</span>
    </div>
    {#each g.items as s (s.id)}
      {@const on = s.id === activeId}
      {@const menuOn = menu?.q.id === s.id}
      <div class="group relative" role="listitem">
        {#if renaming === s.id}
          <div class="flex h-[42px] items-center gap-2 rounded-md bg-active px-2">
            <FileCode size={13} class="shrink-0 text-accent" />
            <input
              class="h-6 min-w-0 flex-1 rounded border border-accent/70 bg-field px-1.5 text-[12.5px] text-ink outline-none"
              value={s.name}
              use:selectOnMount
              aria-label="Query name"
              autocapitalize="off"
              autocorrect="off"
              spellcheck="false"
              autocomplete="off"
              onblur={(e) => finishRename(s, e.currentTarget.value)}
              onkeydown={(e) => { e.stopPropagation(); if (e.key === 'Enter') e.currentTarget.blur(); else if (e.key === 'Escape') cancelRename(s); }}
            />
          </div>
        {:else}
          <button
            data-saved={s.id}
            class="flex w-full items-start gap-2 rounded-md py-1.5 pr-7 pl-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-accent/40 {on || menuOn ? 'bg-active' : 'hover:bg-hover'}"
            title="{s.sql.length > 600 ? s.sql.slice(0, 600) + '…' : s.sql}"
            aria-current={on ? 'true' : undefined}
            use:focusOn={focusRequest?.id === s.id ? focusRequest.n : 0}
            onclick={() => onopen(s)}
            ondblclick={() => startRename(s)}
            oncontextmenu={(e) => openMenu(e, s)}
            onkeydown={(e) => rowKey(e, s)}
          >
            <FileCode size={13} class="mt-[3px] shrink-0 {on ? 'text-accent' : 'text-faint'}" />
            <span class="min-w-0 flex-1">
              <span class="flex items-center gap-1.5">
                <span class="truncate text-[12.5px] {on ? 'font-medium text-ink' : 'text-ink/90'}">{s.name}</span>
                {#if dirty.has(s.id)}<span class="size-1.5 shrink-0 rounded-full bg-accent" title="Unsaved changes in an open tab"></span><span class="sr-only">, unsaved changes</span>{/if}
              </span>
              <span class="flex items-center gap-1.5 text-[11px] text-faint">
                <span class="min-w-0 flex-1 truncate font-mono">{preview(s.sql) || 'Empty'}</span>
                <span class="shrink-0">{ago(s.updated, { year: true })}</span>
              </span>
            </span>
          </button>
          <button
            class="absolute top-1.5 right-1 grid size-5 place-items-center rounded text-faint hover:bg-active hover:text-ink {menuOn ? 'bg-active text-ink' : 'opacity-0 group-hover:opacity-100 focus-visible:opacity-100'}"
            title="Query actions"
            aria-label="Actions for {s.name}"
            onclick={(e) => toggleMenu(e, s)}
          >
            <Ellipsis size={13} />
          </button>
        {/if}
      </div>
    {/each}
  {/each}
</div>

{#if !saved.list.length}
  <div class="mx-1 mt-2 rounded-lg border border-dashed border-line px-3 py-5 text-center">
    <div class="mx-auto mb-2 grid size-8 place-items-center rounded-lg bg-accent/12 text-accent"><BookmarkPlus size={15} /></div>
    <p class="text-[12.5px] font-medium text-ink">No saved queries</p>
    <p class="mt-1 text-[12px] leading-relaxed text-faint">Press <kbd class="kbd">{KEYS.save.hint}</kbd> in the editor to save the query. It stays here for the next session.</p>
  </div>
{:else if !shown}
  <p class="px-2 py-3 text-center text-[12px] text-faint">No query matches “{q}”</p>
{/if}

{#if menu}
  {@const m = menu}
  <div class="popover fixed z-40 w-52 p-1" style="left:{m.x}px;top:{m.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    <div class="truncate px-2 pt-1 pb-1 text-[11.5px] text-faint">{m.q.name}</div>
    <button class="nav-item" role="menuitem" onclick={() => act(onopen)}><FileCode size={13} /> Open</button>
    <button class="nav-item" role="menuitem" onclick={() => act(onrun)}><Play size={13} /> <span class="flex-1">Open and run</span><kbd class="kbd">{KEYS.run.hint}</kbd></button>
    <div class="mx-2 my-1 h-px bg-line"></div>
    <button class="nav-item" role="menuitem" onclick={() => act(startRename)}><Pencil size={13} /> <span class="flex-1">Rename</span><kbd class="kbd">F2</kbd></button>
    <button class="nav-item" role="menuitem" onclick={() => act(duplicate)}><CopyPlus size={13} /> Duplicate</button>
    <button class="nav-item" role="menuitem" onclick={() => act((s) => navigator.clipboard?.writeText(s.sql).catch(() => {}))}><Copy size={13} /> Copy SQL</button>
    <div class="mx-2 my-1 h-px bg-line"></div>
    <button class="nav-item text-red-600 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300" role="menuitem" onclick={() => act(ondelete)}><Trash size={13} /> <span class="flex-1">Delete</span><kbd class="kbd">⌦</kbd></button>
  </div>
{/if}
