<script lang="ts">
  import { ChevronDown, Check, LoaderCircle } from '@lucide/svelte';
  import { catalog, loadDatabases } from '../catalog.svelte';
  import { db, engines, envDot, envTag, canWrite, type Conn } from '../connections.svelte';
  import ReadOnlyBadge from './ReadOnlyBadge.svelte';

  // The parent owns the switch: it asks about unsaved edits and pending transactions first.
  let { conn, database, onconn, ondb }: { conn: Conn; database: string; onconn: (c: Conn) => void; ondb: (name: string) => void } = $props();

  let menu = $state<'conn' | 'db' | null>(null);
  // The catalog keeps the list per connection; every open refetches it so new databases show up.
  const list = $derived(catalog.databases[conn.id]);
  const loading = $derived(list?.status === 'loading');
  const loadError = $derived(list?.status === 'error' ? list.error : null);

  async function openDbs() {
    if (menu === 'db') return (menu = null);
    menu = 'db';
    await loadDatabases(conn.id, true);
  }

  // The tab's database stays pickable even when the list leaves it out (no access to list, or not loaded yet).
  const dbList = $derived.by(() => {
    const l = list?.items ?? [];
    return l.includes(database) ? l : [database, ...l];
  });

  function pickConn(c: Conn) {
    menu = null;
    if (c.id !== conn.id) onconn(c);
  }

  function pickDb(name: string) {
    menu = null;
    if (name !== database) ondb(name);
  }
</script>

<svelte:window onkeydown={(e) => { if (menu && e.key === 'Escape') menu = null; }} />

<!-- shrink-[4]: in a narrow toolbar this gives up width before the transaction buttons next to it. -->
<div class="flex min-w-0 shrink-[4] items-center text-[13px]">
  <div class="relative max-w-72 min-w-0">
    <button class="flex h-7 max-w-full min-w-0 items-center gap-2 overflow-hidden rounded-md px-2 hover:bg-hover {menu === 'conn' ? 'bg-hover' : ''}" title="Connection: {conn.name}{conn.env === 'production' ? ' (production)' : ''}" aria-expanded={menu === 'conn'} onclick={() => (menu = menu === 'conn' ? null : 'conn')}>
      <!-- Production is the main safety cue while typing writes by hand, so it is spelled out, not only a red dot.
           It comes first so a narrow toolbar truncates the name, never the cue. -->
      {#if conn.env === 'production'}<span class="tag -ml-0.5 shrink-0 {envTag.production}">production</span>{:else}<span class="size-1.5 shrink-0 rounded-full {envDot[conn.env]}" title={conn.env}></span><span class="sr-only">{conn.env}</span>{/if}
      <span class="min-w-0 truncate font-medium text-ink">{conn.name}</span>
      {#if !canWrite(conn)}<ReadOnlyBadge />{/if}
      <ChevronDown size={13} class="-ml-0.5 shrink-0 text-faint" />
    </button>
    {#if menu === 'conn'}
      <div class="popover absolute top-full left-0 z-50 mt-1 w-64 p-1">
        <div class="label px-2 pt-1.5 pb-1">Connections</div>
        {#each db.connections as c (c.id)}
          <button class="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left hover:bg-hover" onclick={() => pickConn(c)}>
            <span class="w-5 shrink-0 font-mono text-[10.5px] font-semibold {engines[c.engine].badge}">{engines[c.engine].short}</span>
            <span class="min-w-0 flex-1 truncate text-ink">{c.name}</span>
            {#if !canWrite(c)}<ReadOnlyBadge />{/if}
            <span class="size-1.5 shrink-0 rounded-full {envDot[c.env]}" title={c.env}></span><span class="sr-only">{c.env}</span>
            <Check size={14} class="shrink-0 text-accent {c.id === conn.id ? '' : 'invisible'}" />
          </button>
        {/each}
      </div>
    {/if}
  </div>

  <!-- SQLite has one database, `main`: nothing to pick. -->
  {#if conn.engine !== 'sqlite'}
    <span class="shrink-0 text-faint/60">/</span>
    <div class="relative max-w-44 min-w-16 shrink-[3]">
      <button class="flex h-7 max-w-full min-w-0 items-center gap-1 overflow-hidden rounded-md px-2 hover:bg-hover {menu === 'db' ? 'bg-hover' : ''}" title="Database: {database}" aria-expanded={menu === 'db'} onclick={openDbs}>
        <span class="min-w-0 truncate font-mono text-[12px] text-muted">{database}</span>
        <ChevronDown size={13} class="shrink-0 text-faint" />
      </button>
      {#if menu === 'db'}
        <div class="popover absolute top-full left-0 z-50 mt-1 max-h-72 w-56 overflow-auto p-1">
          <div class="label flex items-center gap-1.5 px-2 pt-1.5 pb-1">
            Databases {#if loading}<LoaderCircle size={11} class="animate-spin" />{/if}
          </div>
          {#each dbList as name (name)}
            <button class="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left hover:bg-hover" onclick={() => pickDb(name)}>
              <span class="min-w-0 flex-1 truncate font-mono text-[12px] text-ink">{name}</span>
              <Check size={14} class="shrink-0 text-accent {name === database ? '' : 'invisible'}" />
            </button>
          {/each}
          {#if loadError}<p class="px-2 py-1 text-[12px] text-red-700 dark:text-red-400" role="alert">{loadError}</p>{/if}
        </div>
      {/if}
    </div>
  {/if}
</div>
{#if menu}
  <!-- Under the open menu (z-50), over everything else. -->
  <button class="fixed inset-0 z-40 cursor-default" aria-label="Close" onclick={() => (menu = null)}></button>
{/if}
