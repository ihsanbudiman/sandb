<script lang="ts">
  import { Check, ChevronDown, ChevronsUpDown, LoaderCircle, RotateCw, Settings } from '@lucide/svelte';
  import { scrollOn } from '../actions';
  import { ui, ai, aiModels, loadAiModels, setAiModel } from '../state.svelte';

  // One model for every chat. `field`: the Settings input, which names the fallback model; `chip`: the small button in the chat composer.
  type Props = { onpick?: () => void } & ({ variant: 'field'; placeholder?: string } | { variant: 'chip' });
  let props: Props = $props();
  const isField = $derived(props.variant === 'field');
  const placeholder = $derived(props.variant === 'field' ? (props.placeholder ?? '') : '');

  let menu = $state(false);
  let filter = $state('');
  let hi = $state(0);

  const query = $derived(filter.trim());
  // "Not listed" only means something once the server has answered with a list.
  const known = $derived(!aiModels.loading && !aiModels.error && aiModels.list.length > 0);
  // The chosen model stays at the top when the server does not list it, so the user sees what is set.
  // A typed name that is not listed comes last, so Enter takes the first listed match.
  const rows = $derived.by(() => {
    const q = query.toLowerCase();
    const listed = aiModels.list.filter((m) => m.toLowerCase().includes(q));
    const all = !q && ai.model && !aiModels.list.includes(ai.model) ? [ai.model, ...listed] : listed;
    const shown = all.map((id) => ({ id, custom: false }));
    return query && !aiModels.list.includes(query) ? [...shown, { id: query, custom: true }] : shown;
  });

  function open() {
    menu = !menu;
    if (!menu) return;
    filter = '';
    loadAiModels();
    hi = Math.max(0, rows.findIndex((r) => r.id === ai.model));
  }

  function choose(id: string) {
    menu = false;
    if (id !== ai.model) setAiModel(id);
    props.onpick?.();
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (!rows.length) return;
      hi = (hi + (e.key === 'ArrowDown' ? 1 : rows.length - 1)) % rows.length;
    } else if (e.key === 'Enter') {
      e.preventDefault();
      // Before the list arrives, a half-typed name would win; wait for the list.
      if (aiModels.loading && !aiModels.list.length) return;
      const r = rows[hi];
      if (r) choose(r.id);
    }
  }
</script>

<svelte:window onkeydown={(e) => { if (menu && e.key === 'Escape') menu = false; }} />

<!-- The chip menu opens upward and takes the width of the nearest positioned parent (the composer row), so a narrow panel does not cut it. -->
<div class={isField ? 'relative w-full' : 'min-w-0 max-w-44'}>
  {#if isField}
    <button type="button" class="input flex w-full items-center gap-2 text-left font-mono text-[12.5px]" aria-haspopup="listbox" aria-expanded={menu} onclick={open}>
      <span class="min-w-0 flex-1 truncate {ai.model ? '' : 'text-faint'}">{ai.model || placeholder || 'Choose a model'}</span>
      <ChevronsUpDown size={13} class="shrink-0 text-faint" />
    </button>
  {:else}
    <button type="button" class="flex h-6 max-w-full min-w-0 items-center gap-1 rounded-md px-1.5 font-mono text-[11.5px] text-muted hover:bg-hover hover:text-ink {menu ? 'bg-hover text-ink' : ''}" aria-haspopup="listbox" aria-expanded={menu} title="Model: {ai.model || 'none'}. Click to change." onclick={open}>
      <span class="min-w-0 truncate">{ai.model || 'Choose a model'}</span>
      <ChevronDown size={11} class="shrink-0" />
    </button>
  {/if}

  {#if menu}
    <div class="popover absolute z-50 flex max-h-80 flex-col p-1 {isField ? 'top-full mt-1 right-0 w-full min-w-80 max-w-[calc(100vw-2rem)]' : 'bottom-full mb-1 inset-x-0'}">
      <div class="mb-1 flex shrink-0 items-center gap-1">
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="input h-7 min-w-0 flex-1 font-mono text-[12px]"
          placeholder="Search or type a model name"
          aria-label="Search or type a model name"
          bind:value={filter}
          oninput={() => (hi = 0)}
          onkeydown={key}
          autofocus
          autocapitalize="off"
          autocorrect="off"
          autocomplete="off"
          spellcheck="false"
        />
        <button type="button" class="btn btn-ghost size-7 shrink-0 p-0" onclick={() => loadAiModels(true)} disabled={aiModels.loading} title="Load the model list again" aria-label="Load the model list again">
          {#if aiModels.loading}<LoaderCircle size={13} class="animate-spin" />{:else}<RotateCw size={13} />{/if}
        </button>
      </div>
      <div class="min-h-0 flex-1 overflow-auto" role="listbox" aria-label="Models">
        {#each rows as r, i (r.custom ? `custom:${r.id}` : r.id)}
          <button
            type="button"
            role="option"
            aria-selected={r.id === ai.model}
            data-hi={i === hi ? '' : undefined}
            class="flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-[12px] {i === hi ? 'bg-hover' : ''}"
            use:scrollOn={i === hi}
            onmousemove={() => (hi = i)}
            onclick={() => choose(r.id)}
          >
            {#if r.custom}
              <span class="shrink-0 text-muted">Use</span>
              <span class="min-w-0 flex-1 truncate font-mono text-ink">{r.id}</span>
              {#if known}<span class="shrink-0 text-[11px] text-faint">not listed</span>{/if}
            {:else}
              <span class="min-w-0 flex-1 truncate font-mono text-ink">{r.id}</span>
              {#if known && !aiModels.list.includes(r.id)}<span class="shrink-0 text-[11px] text-faint">not listed</span>{/if}
              {#if r.id === ai.model}<Check size={12} class="shrink-0 text-accent" />{/if}
            {/if}
          </button>
        {/each}
        {#if aiModels.loading && !aiModels.list.length}
          <p class="flex items-center gap-1.5 px-2 py-1.5 text-[12px] text-muted"><LoaderCircle size={12} class="animate-spin" /> Loading models…</p>
        {:else if aiModels.error}
          <p class="px-2 py-1.5 text-[12px] break-words text-red-700 dark:text-red-400" role="alert">Could not load the models: {aiModels.error}</p>
          <p class="px-2 pb-1.5 text-[11.5px] text-faint">You can still type a model name above and press Enter.</p>
        {:else if !rows.length}
          <p class="px-2 py-1.5 text-[12px] text-muted">{aiModels.list.length ? 'No model matches.' : 'The server lists no models. Type a model name above.'}</p>
        {/if}
      </div>
      {#if !isField}
        <button type="button" class="mt-1 flex h-7 shrink-0 items-center gap-2 rounded-md border-t border-line px-2 pt-1 text-left text-[12px] text-muted hover:text-ink" onclick={() => { menu = false; ui.settingsSection = 'ai'; ui.page = 'settings'; }}>
          <Settings size={12} /> Provider settings…
        </button>
      {:else if aiModels.list.length}
        <p class="shrink-0 border-t border-line px-2 pt-1.5 pb-1 text-[11px] text-faint">{aiModels.list.length.toLocaleString()} {aiModels.list.length === 1 ? 'model' : 'models'} on this server.</p>
      {/if}
    </div>
  {/if}
</div>
{#if menu}
  <!-- Under the open menu (z-50), over everything else. -->
  <button type="button" class="fixed inset-0 z-40 cursor-default" aria-label="Close" onclick={() => (menu = false)}></button>
{/if}
