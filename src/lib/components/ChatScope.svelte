<script lang="ts">
  import type { Snippet } from 'svelte';
  import { Plus, X, Check, ChevronRight, LoaderCircle } from '@lucide/svelte';
  import { catalog, loadDatabases } from '../catalog.svelte';
  import { db, defaultDb, envDot, connOf, dbOf, sameSource, type Conn, type Source } from '../connections.svelte';
  import { chatDatabaseLabel } from '../chatParts';
  import { ui } from '../state.svelte';
  import { MAX_SOURCES } from '../api/generated/constants';
  import { focusOnMount } from '../actions';
  import EngineBadge from './ui/EngineBadge.svelte';
  import { placePopup } from './ui/popup';

  let {
    sources,
    disabled = false,
    onchange,
    placement = 'up',
    note = 'A change applies from the next question.',
    children,
  }: { sources: Source[]; disabled?: boolean; onchange: (list: Source[]) => void; placement?: 'up' | 'down'; note?: string; children?: Snippet } = $props();

  let menu = $state(false);
  let trigger = $state<HTMLButtonElement>();
  const pickerId = $props.id();
  let filter = $state('');
  const expanded = $state<Record<string, boolean>>({});

  $effect(() => {
    if (disabled || (placement === 'up' && !ui.chatOpen)) menu = false;
  });

  const full = $derived(sources.length >= MAX_SOURCES);
  const query = $derived(filter.trim().toLowerCase());

  async function load(c: Conn) {
    if (c.engine === 'sqlite') return;
    await loadDatabases(c.id);
  }

  function toggleConn(c: Conn) {
    expanded[c.id] = !expanded[c.id];
    if (expanded[c.id]) load(c);
  }

  // The chosen databases and the connection's own one stay in the list when the server list leaves them out or is not loaded.
  function dbList(c: Conn) {
    const chosen = sources.filter((s) => s.connId === c.id).map((s) => dbOf(s, c));
    // An empty connection database (possible on MongoDB) is not a choice.
    const all = [...new Set([defaultDb(c), ...(catalog.databases[c.id]?.items ?? []), ...chosen])].filter((n) => n !== '');
    return query && !c.name.toLowerCase().includes(query) ? all.filter((n) => n.toLowerCase().includes(query) || chatDatabaseLabel(c, n).toLowerCase().includes(query)) : all;
  }

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

  function dismiss() {
    menu = false;
  }

  function pickerKey(e: KeyboardEvent) {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    e.stopPropagation();
    dismiss();
    trigger?.focus({ preventScroll: true });
  }
</script>

{#snippet picker()}
  <div class={placement === 'up' ? 'sticky -top-1 z-sticky -m-1 mb-0 bg-surface p-1' : 'mb-1 shrink-0'}>
    <input class="input input-sm" placeholder="Filter connections and databases" aria-label="Filter connections and databases" bind:value={filter} use:focusOnMount autocapitalize="off" autocorrect="off" autocomplete="off" spellcheck="false" />
  </div>
  <div class={placement === 'down' ? 'min-h-0 flex-1 overflow-auto' : ''}>
    {#each shown as c (c.id)}
      {@const n = chosenIn(c)}
      {#if c.engine === 'sqlite'}
        {@const own = defaultDb(c)}
        {@const on = has(c, own)}
        <button type="button" class="menu-item" disabled={!on && full} onclick={() => toggle(c, own)} aria-pressed={on}>
          <span class="w-3"></span>
          <EngineBadge engine={c.engine} class="w-5" />
          <span class="min-w-0 flex-1 truncate text-ink">{c.name}</span>
          <span class="grid size-3.5 shrink-0 place-items-center rounded-sm border {on ? 'border-accent bg-accent text-accent-fg' : 'border-line'}">{#if on}<Check size={12} />{/if}</span>
        </button>
      {:else}
        {@const openRow = expanded[c.id] || !!query}
        <button type="button" class="menu-item" aria-expanded={openRow} onclick={() => toggleConn(c)}>
          <ChevronRight size={12} class="w-3 shrink-0 text-faint motion-safe:transition-transform motion-safe:duration-150 {openRow ? 'rotate-90' : ''}" />
          <EngineBadge engine={c.engine} class="w-5" />
          <span class="min-w-0 flex-1 truncate text-ink">{c.name}</span>
          {#if catalog.databases[c.id]?.status === 'loading'}<LoaderCircle size={12} class="shrink-0 text-faint motion-safe:animate-spin" />{/if}
          {#if n}<span class="shrink-0 text-xs text-accent tabular-nums">{n}</span>{/if}
        </button>
        {#if openRow}
          {#each dbList(c) as name (name)}
            {@const on = has(c, name)}
            <button type="button" class="menu-item pl-12" disabled={!on && full} onclick={() => toggle(c, name)} aria-pressed={on}>
              <span class="min-w-0 flex-1 truncate font-mono text-ink">{chatDatabaseLabel(c, name)}</span>
              <span class="grid size-3.5 shrink-0 place-items-center rounded-sm border {on ? 'border-accent bg-accent text-accent-fg' : 'border-line'}">{#if on}<Check size={12} />{/if}</span>
            </button>
          {/each}
          {#if catalog.databases[c.id]?.status === 'error'}<p class="py-1 pr-2 pl-12 text-sm text-danger" role="alert">{catalog.databases[c.id].error}</p>{/if}
        {/if}
      {/if}
    {:else}
      <p class="px-2 py-1.5 text-sm text-muted">{conns.length ? 'Nothing matches the filter.' : 'Add a connection first.'}</p>
    {/each}
  </div>
  <p class="shrink-0 border-t border-line px-2 pt-1.5 pb-1 text-xs text-faint">{sources.length} of {MAX_SOURCES} databases.{note ? ` ${note}` : ''}</p>
{/snippet}

<div class="flex flex-wrap items-center gap-1 pb-1.5">
  {#each sources as s, i (i)}
    {@const c = connOf(s)}
    {@const database = chatDatabaseLabel(c, dbOf(s, c))}
    <span class="chip min-w-0 pr-0.5" title={c ? `${c.name}${c.engine === 'sqlite' ? '' : ` / ${database}`} (${c.env})` : 'This connection was deleted'}>
      {#if c}
        <span class="size-1.5 shrink-0 rounded-full {envDot[c.env]}" title={c.env}></span><span class="sr-only">{c.env}</span>
        <span class="min-w-0 truncate text-ink">{c.name}</span>
        {#if c.engine !== 'sqlite'}<span class="text-faint">/</span><span class="min-w-0 truncate font-mono text-muted">{database}</span>{/if}
      {:else}
        <span class="truncate text-faint italic">Deleted connection</span>
      {/if}
      <button type="button" class="btn btn-sm btn-ghost size-4.5 shrink-0 p-0" {disabled} onclick={() => onchange(sources.filter((_, j) => j !== i))} aria-label="Remove {c ? `${c.name} ${database}` : 'deleted connection'} from the chat"><X size={12} /></button>
    </span>
  {/each}
  {@render children?.()}
  <button type="button" bind:this={trigger} class="btn btn-sm btn-ghost px-1.5 {menu ? 'bg-active text-ink' : ''}" {disabled} aria-expanded={menu} aria-controls={menu ? pickerId : undefined} title={disabled ? 'Wait for the reply to finish' : 'Choose the databases that the assistant can use'} onclick={open}>
    <Plus size={12} /> {sources.length ? 'Database' : 'Add a database'}
  </button>
</div>

{#if menu}
  {#if placement === 'down'}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions (Delegates Escape from the picker's controls.) -->
    <div id={pickerId} class="popover mt-1 flex max-h-80 w-full flex-col p-1" role="group" aria-label="Chat databases" onkeydown={pickerKey}>
      {@render picker()}
    </div>
  {:else if trigger}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions (Delegates Escape from the picker's controls.) -->
    <div id={pickerId} class="popover fixed z-menu w-72 overflow-auto p-1 motion-safe:animate-popover-in" role="group" aria-label="Chat databases" use:placePopup={{ anchor: trigger, maxHeight: 320, dismiss }} onkeydown={pickerKey}>
      {@render picker()}
    </div>
  {/if}
{/if}
