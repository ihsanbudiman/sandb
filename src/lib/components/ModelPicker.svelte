<script lang="ts">
  import { Check, ChevronDown, ChevronsUpDown, LoaderCircle, RotateCw, Settings } from '@lucide/svelte';
  import { focusOnMount, scrollOn } from '../actions';
  import { ui, ai, aiModels, loadAiModels, setAiModel } from '../state.svelte';
  import { placePopup } from './ui/popup';

  // One model for every chat. `field`: the Settings input, which names the fallback model; `chip`: the small button in the chat composer.
  type Props = { onpick?: () => void } & ({ variant: 'field'; placeholder?: string } | { variant: 'chip' });
  let props: Props = $props();
  const isField = $derived(props.variant === 'field');
  const placeholder = $derived(props.variant === 'field' ? (props.placeholder ?? '') : '');

  let menu = $state(false);
  let trigger = $state<HTMLButtonElement>();
  const pickerId = $props.id();
  let filter = $state('');
  let hi = $state(0);

  $effect(() => {
    if (!isField && !ui.chatOpen) menu = false;
  });

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

  function close(refocus = false) {
    menu = false;
    if (refocus) trigger?.focus({ preventScroll: true });
  }

  function pickerKey(e: KeyboardEvent) {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    e.stopPropagation();
    close(true);
  }

  function choose(id: string) {
    close(true);
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

<div class={isField ? 'w-full' : 'min-w-0 flex-1'}>
  {#if isField}
    <button type="button" bind:this={trigger} class="input input-sm focus-ring flex w-full items-center gap-2 text-left font-mono" aria-expanded={menu} aria-controls={menu ? pickerId : undefined} onclick={open}>
      <span class="min-w-0 flex-1 truncate {ai.model ? '' : 'text-faint'}">{ai.model || placeholder || 'Choose a model'}</span>
      <ChevronsUpDown size={13} class="shrink-0 text-faint" />
    </button>
  {:else}
    <button type="button" bind:this={trigger} class="btn btn-sm btn-ghost ml-auto flex w-fit max-w-full min-w-0 gap-1 px-1.5 font-mono font-normal {menu ? 'bg-active text-ink' : ''}" aria-expanded={menu} aria-controls={menu ? pickerId : undefined} title="Model: {ai.model || 'none'}. Click to change." onclick={open}>
      <span class="min-w-0 truncate">{ai.model || 'Choose a model'}</span>
      <ChevronDown size={12} class="shrink-0" />
    </button>
  {/if}

  {#if menu && trigger}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions (Delegates Escape from the picker's controls.) -->
    <div id={pickerId} class="popover fixed z-menu w-80 overflow-auto p-1 scroll-pt-10 motion-safe:animate-popover-in" role="group" aria-label="Model picker" use:placePopup={{ anchor: trigger, align: 'end', matchWidth: isField, maxHeight: 320, dismiss: close }} onkeydown={pickerKey}>
      <div class="sticky -top-1 z-sticky -m-1 mb-0 flex items-center gap-1 bg-surface p-1">
        <input
          class="input input-sm min-w-0 flex-1 font-mono"
          placeholder="Search or type a model name"
          role="combobox"
          aria-expanded="true"
          aria-autocomplete="list"
          aria-controls={`${pickerId}-models`}
          aria-activedescendant={rows[hi] ? `${pickerId}-model-${hi}` : undefined}
          aria-label="Search or type a model name"
          bind:value={filter}
          oninput={() => (hi = 0)}
          onkeydown={key}
          use:focusOnMount
          autocapitalize="off"
          autocorrect="off"
          autocomplete="off"
          spellcheck="false"
        />
        <button type="button" class="btn btn-ghost size-7 shrink-0 p-0" onclick={() => loadAiModels(true)} disabled={aiModels.loading} title="Load the model list again" aria-label="Load the model list again">
          {#if aiModels.loading}<LoaderCircle size={13} class="motion-safe:animate-spin" />{:else}<RotateCw size={13} />{/if}
        </button>
      </div>
      <div id={`${pickerId}-models`} role="listbox" aria-label="Models">
        {#each rows as r, i (r.custom ? `custom:${r.id}` : r.id)}
          <button
            type="button"
            id={`${pickerId}-model-${i}`}
            role="option"
            tabindex="-1"
            aria-selected={r.id === ai.model}
            class="menu-item {i === hi ? 'bg-active text-ink' : ''}"
            use:scrollOn={i === hi}
            onmousemove={() => (hi = i)}
            onclick={() => choose(r.id)}
          >
            {#if r.custom}
              <span class="shrink-0 text-muted">Use</span>
              <span class="min-w-0 flex-1 truncate font-mono text-ink">{r.id}</span>
              {#if known}<span class="shrink-0 text-xs text-faint">not listed</span>{/if}
            {:else}
              <span class="min-w-0 flex-1 truncate font-mono text-ink">{r.id}</span>
              {#if known && !aiModels.list.includes(r.id)}<span class="shrink-0 text-xs text-faint">not listed</span>{/if}
              {#if r.id === ai.model}<Check size={12} class="shrink-0 text-accent" />{/if}
            {/if}
          </button>
        {/each}
      </div>
      {#if aiModels.loading && !aiModels.list.length}
        <p class="flex items-center gap-1.5 px-2 py-1.5 text-sm text-muted"><LoaderCircle size={13} class="motion-safe:animate-spin" /> Loading models…</p>
      {:else if aiModels.error}
        <p class="px-2 py-1.5 text-sm break-words text-danger" role="alert">Could not load the models: {aiModels.error}</p>
        <p class="px-2 pb-1.5 text-xs text-faint">You can still type a model name above and press Enter.</p>
      {:else if !rows.length}
        <p class="px-2 py-1.5 text-sm text-muted">{aiModels.list.length ? 'No model matches.' : 'The server lists no models. Type a model name above.'}</p>
      {/if}
      {#if !isField}
        <button type="button" class="menu-item mt-1 shrink-0 border-t border-line" onclick={() => { close(); ui.settingsSection = 'ai'; ui.page = 'settings'; }}>
          <Settings size={12} /> Provider settings…
        </button>
      {:else if aiModels.list.length}
        <p class="shrink-0 border-t border-line px-2 pt-1.5 pb-1 text-xs text-faint">{aiModels.list.length.toLocaleString()} {aiModels.list.length === 1 ? 'model' : 'models'} on this server.</p>
      {/if}
    </div>
  {/if}
</div>
