<script lang="ts">
  import { Plus, X, Check, ChevronRight, LoaderCircle } from '@lucide/svelte';
  import { db, engines, envDot, type Conn } from '../state.svelte';
  import { defaultDb } from '../tabs.svelte';
  import { api, ApiError } from '../api';
  import { connOf, dbOf, sameSource, MAX_SOURCES, type Source } from '../chat.svelte';

  // The databases of one chat: chips, and a menu to add or remove databases of any connection.
  let { sources, disabled = false, onchange }: { sources: Source[]; disabled?: boolean; onchange: (list: Source[]) => void } = $props();

  let menu = $state(false);
  let filter = $state('');
  const expanded = $state<Record<string, boolean>>({});
  // Per connection; loaded when the connection opens in the menu.
  const dbs = $state<Record<string, string[]>>({});
  const loading = $state<Record<string, boolean>>({});
  const errors = $state<Record<string, string>>({});

  const full = $derived(sources.length >= MAX_SOURCES);
  const query = $derived(filter.trim().toLowerCase());

  async function load(c: Conn) {
    if (c.engine === 'sqlite' || loading[c.id]) return;
    loading[c.id] = true;
    delete errors[c.id];
    try {
      dbs[c.id] = await api.listDatabases(c.id);
    } catch (e) {
      errors[c.id] = ApiError.from(e).message;
    } finally {
      loading[c.id] = false;
    }
  }

  function toggleConn(c: Conn) {
    expanded[c.id] = !expanded[c.id];
    if (expanded[c.id]) load(c);
  }

  // The chosen databases and the connection's own one stay in the list when the server list leaves them out or is not loaded.
  function dbList(c: Conn) {
    const chosen = sources.filter((s) => s.connId === c.id).map((s) => dbOf(s, c));
    // An empty connection database (possible on MongoDB) is not a choice.
    const all = [...new Set([defaultDb(c), ...(dbs[c.id] ?? []), ...chosen])].filter((n) => n !== '');
    return query && !c.name.toLowerCase().includes(query) ? all.filter((n) => n.toLowerCase().includes(query)) : all;
  }

  // SQL and MongoDB databases; the engine badge tells them apart. A Mongo source always names a database.
  const conns = $derived(db.connections);
  const shown = $derived(query ? conns.filter((c) => c.name.toLowerCase().includes(query) || dbList(c).length > 0) : conns);

  const has = (c: Conn, name: string) => sources.some((s) => sameSource(s, { connId: c.id, database: name }));
  const chosenIn = (c: Conn) => sources.filter((s) => s.connId === c.id).length;

  function toggle(c: Conn, name: string) {
    const src = { connId: c.id, database: name };
    if (has(c, name)) onchange(sources.filter((s) => !sameSource(s, src)));
    else if (!full) onchange([...sources, src]);
  }

  function open() {
    menu = !menu;
    filter = '';
    // Connections that already have a chosen database open with it, so the user sees the choice.
    if (menu) for (const c of conns) if (c.engine !== 'sqlite' && chosenIn(c) && expanded[c.id] === undefined) toggleConn(c);
  }
</script>

<svelte:window onkeydown={(e) => { if (menu && e.key === 'Escape') menu = false; }} />

<div class="relative flex flex-wrap items-center gap-1 pb-1.5">
  {#each sources as s, i (i)}
    {@const c = connOf(s)}
    <span class="flex h-6 max-w-full min-w-0 items-center gap-1 rounded-md border border-line bg-surface pr-0.5 pl-1.5 text-[11.5px]" title={c ? `${c.name}${c.engine === 'sqlite' ? '' : ` / ${dbOf(s, c)}`} (${c.env})` : 'This connection was deleted'}>
      {#if c}
        <span class="size-1.5 shrink-0 rounded-full {envDot[c.env]}" title={c.env}></span><span class="sr-only">{c.env}</span>
        <span class="min-w-0 truncate text-ink">{c.name}</span>
        {#if c.engine !== 'sqlite'}<span class="text-faint">/</span><span class="min-w-0 truncate font-mono text-muted">{dbOf(s, c)}</span>{/if}
      {:else}
        <span class="truncate text-faint italic">Deleted connection</span>
      {/if}
      <button class="grid size-4.5 shrink-0 place-items-center rounded text-faint hover:bg-hover hover:text-ink disabled:opacity-40" {disabled} onclick={() => onchange(sources.filter((_, j) => j !== i))} aria-label="Remove {c ? `${c.name} ${dbOf(s, c)}` : 'deleted connection'} from the chat"><X size={11} /></button>
    </span>
  {/each}
  <button class="flex h-6 items-center gap-1 rounded-md px-1.5 text-[11.5px] text-muted hover:bg-hover hover:text-ink disabled:opacity-40 {menu ? 'bg-hover text-ink' : ''}" {disabled} aria-expanded={menu} title={disabled ? 'Wait for the reply to finish' : 'Choose the databases that the assistant can use'} onclick={open}>
    <Plus size={12} /> {sources.length ? 'Database' : 'Add a database'}
  </button>

  {#if menu}
    <div class="popover absolute bottom-full left-0 z-50 mb-1 flex max-h-80 w-full min-w-60 flex-col p-1">
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input mb-1 h-7 shrink-0 text-[12px]" placeholder="Filter connections and databases" aria-label="Filter connections and databases" bind:value={filter} autofocus autocapitalize="off" autocorrect="off" autocomplete="off" spellcheck="false" />
      <div class="min-h-0 flex-1 overflow-auto">
        {#each shown as c (c.id)}
          {@const n = chosenIn(c)}
          {#if c.engine === 'sqlite'}
            {@const on = has(c, 'main')}
            <button class="flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-[12.5px] hover:bg-hover disabled:opacity-40" disabled={!on && full} onclick={() => toggle(c, 'main')} aria-pressed={on}>
              <span class="w-3"></span>
              <span class="w-5 shrink-0 font-mono text-[10.5px] font-semibold {engines[c.engine].badge}">{engines[c.engine].short}</span>
              <span class="min-w-0 flex-1 truncate text-ink">{c.name}</span>
              <span class="grid size-3.5 shrink-0 place-items-center rounded-sm border {on ? 'border-accent bg-accent text-white' : 'border-line'}">{#if on}<Check size={10} />{/if}</span>
            </button>
          {:else}
            {@const openRow = expanded[c.id] || !!query}
            <button class="flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-[12.5px] hover:bg-hover" aria-expanded={openRow} onclick={() => toggleConn(c)}>
              <ChevronRight size={12} class="w-3 shrink-0 text-faint transition-transform {openRow ? 'rotate-90' : ''}" />
              <span class="w-5 shrink-0 font-mono text-[10.5px] font-semibold {engines[c.engine].badge}">{engines[c.engine].short}</span>
              <span class="min-w-0 flex-1 truncate text-ink">{c.name}</span>
              {#if loading[c.id]}<LoaderCircle size={11} class="shrink-0 animate-spin text-faint" />{/if}
              {#if n}<span class="shrink-0 text-[11px] text-accent tabular-nums">{n}</span>{/if}
            </button>
            {#if openRow}
              {#each dbList(c) as name (name)}
                {@const on = has(c, name)}
                <button class="flex h-7 w-full items-center gap-2 rounded-md pr-2 pl-12 text-left hover:bg-hover disabled:opacity-40" disabled={!on && full} onclick={() => toggle(c, name)} aria-pressed={on}>
                  <span class="min-w-0 flex-1 truncate font-mono text-[12px] text-ink">{name}</span>
                  <span class="grid size-3.5 shrink-0 place-items-center rounded-sm border {on ? 'border-accent bg-accent text-white' : 'border-line'}">{#if on}<Check size={10} />{/if}</span>
                </button>
              {/each}
              {#if errors[c.id]}<p class="py-1 pr-2 pl-12 text-[12px] text-red-700 dark:text-red-400" role="alert">{errors[c.id]}</p>{/if}
            {/if}
          {/if}
        {:else}
          <p class="px-2 py-1.5 text-[12px] text-muted">{conns.length ? 'Nothing matches the filter.' : 'Add a connection first.'}</p>
        {/each}
      </div>
      <p class="shrink-0 border-t border-line px-2 pt-1.5 pb-1 text-[11px] text-faint">{sources.length} of {MAX_SOURCES} databases. A change applies from the next question.</p>
    </div>
  {/if}
</div>
{#if menu}
  <!-- Under the open menu (z-50), over everything else. -->
  <button class="fixed inset-0 z-40 cursor-default" aria-label="Close" onclick={() => (menu = false)}></button>
{/if}
