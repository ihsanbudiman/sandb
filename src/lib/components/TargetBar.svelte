<script lang="ts">
  import { ChevronDown, Check, LoaderCircle } from '@lucide/svelte';
  import { catalog, loadDatabases } from '../catalog.svelte';
  import { db, envDot, envTag, canWrite, type Conn } from '../connections.svelte';
  import ReadOnlyBadge from './ReadOnlyBadge.svelte';
  import EngineBadge from './ui/EngineBadge.svelte';
  import Menu from './ui/Menu.svelte';

  // The parent owns the switch: it asks about unsaved edits and pending transactions first.
  let { conn, database, onconn, ondb, switching = null }: {
    conn: Conn;
    database: string;
    onconn: (c: Conn) => void;
    ondb: (name: string) => void;
    switching?: 'conn' | 'db' | null;
  } = $props();

  let menu = $state<'conn' | 'db' | null>(null);
  let connTrigger = $state<HTMLButtonElement>();
  let dbTrigger = $state<HTMLButtonElement>();
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

<!-- shrink-[4]: in a narrow toolbar this gives up width before the transaction buttons next to it. -->
<div class="flex min-w-0 shrink-[4] items-center text-base">
  <div class="max-w-72 min-w-0">
    <button bind:this={connTrigger} class="btn btn-ghost h-7 max-w-full min-w-0 gap-2 overflow-hidden px-2 {menu === 'conn' ? 'bg-hover' : ''}" title="Connection: {conn.name}{conn.env === 'production' ? ' (production)' : ''}" aria-haspopup="menu" aria-expanded={menu === 'conn'} aria-busy={switching === 'conn'} disabled={!!switching} onclick={() => (menu = menu === 'conn' ? null : 'conn')}>
      <!-- Production is the main safety cue while typing writes by hand, so it is spelled out, not only a red dot.
           It comes first so a narrow toolbar truncates the name, never the cue. -->
      {#if conn.env === 'production'}<span class="tag -ml-0.5 shrink-0 {envTag.production}">production</span>{:else}<span class="size-1.5 shrink-0 rounded-full {envDot[conn.env]}" title={conn.env}></span><span class="sr-only">{conn.env}</span>{/if}
      <span class="min-w-0 truncate font-medium text-ink">{conn.name}</span>
      {#if !canWrite(conn)}<ReadOnlyBadge />{/if}
      {#if switching === 'conn'}<LoaderCircle size={13} class="-ml-0.5 shrink-0 text-faint motion-safe:animate-spin" />{:else}<ChevronDown size={13} class="-ml-0.5 shrink-0 text-faint" />{/if}
    </button>
    {#if menu === 'conn' && connTrigger}
      <Menu anchor={connTrigger} label="Connections" class="w-64" onclose={() => (menu = null)}>
        <div class="label px-2 pt-1.5 pb-1">Connections</div>
        {#each db.connections as c (c.id)}
          <button type="button" role="menuitemradio" tabindex="-1" aria-checked={c.id === conn.id} class="menu-item" onclick={() => pickConn(c)}>
            <EngineBadge engine={c.engine} class="w-5" />
            <span class="min-w-0 flex-1 truncate text-ink">{c.name}</span>
            {#if !canWrite(c)}<ReadOnlyBadge />{/if}
            <span class="size-1.5 shrink-0 rounded-full {envDot[c.env]}" title={c.env}></span><span class="sr-only">{c.env}</span>
            <Check size={13} class="shrink-0 text-accent {c.id === conn.id ? '' : 'invisible'}" />
          </button>
        {/each}
      </Menu>
    {/if}
  </div>

  <!-- SQLite has one database, `main`: nothing to pick. -->
  {#if conn.engine !== 'sqlite'}
    <span class="shrink-0 text-faint/60">/</span>
    <div class="max-w-44 min-w-16 shrink-[3]">
      <button bind:this={dbTrigger} class="btn btn-ghost h-7 max-w-full min-w-0 gap-1 overflow-hidden px-2 {menu === 'db' ? 'bg-hover' : ''}" title="Database: {database}" aria-haspopup="menu" aria-expanded={menu === 'db'} aria-busy={switching === 'db'} disabled={!!switching} onclick={openDbs}>
        <span class="min-w-0 truncate font-mono font-normal text-muted">{database}</span>
        {#if switching === 'db'}<LoaderCircle size={13} class="shrink-0 text-faint motion-safe:animate-spin" />{:else}<ChevronDown size={13} class="shrink-0 text-faint" />{/if}
      </button>
      {#if menu === 'db' && dbTrigger}
        <Menu anchor={dbTrigger} label="Databases" class="w-56" onclose={() => (menu = null)}>
          <div class="label flex items-center gap-1.5 px-2 pt-1.5 pb-1">
            Databases {#if loading}<LoaderCircle size={12} class="motion-safe:animate-spin" />{/if}
          </div>
          {#each dbList as name (name)}
            <button type="button" role="menuitemradio" tabindex="-1" aria-checked={name === database} class="menu-item" onclick={() => pickDb(name)}>
              <span class="min-w-0 flex-1 truncate font-mono text-ink">{name}</span>
              <Check size={13} class="shrink-0 text-accent {name === database ? '' : 'invisible'}" />
            </button>
          {/each}
          {#if loadError}<p class="px-2 py-1 text-sm text-danger" role="alert">{loadError}</p>{/if}
        </Menu>
      {/if}
    </div>
  {/if}
</div>
