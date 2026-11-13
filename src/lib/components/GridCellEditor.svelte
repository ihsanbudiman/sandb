<script lang="ts">
  import { selectOnMount } from '../actions';

  let {
    mongo,
    value = $bindable(''),
    type = $bindable('string'),
    types = [],
    error,
    oncommit,
    oncancel,
    oninput,
  }: {
    mongo: boolean;
    value?: string;
    type?: string;
    types?: string[];
    error: string | null;
    /** Enter saves; `blur` says the cell lost focus, where an invalid draft is dropped instead of kept. */
    oncommit: (blur: boolean) => void;
    oncancel: () => void;
    /** Typing clears the error; a null cell picks a type that can hold the text. */
    oninput?: () => void;
  } = $props();

  let input = $state<HTMLInputElement>();
  let select = $state<HTMLSelectElement>();
  /** A pointer press on the type list; the input's blur that follows must not commit. */
  let picking = false;
</script>

<input
  class="absolute inset-0 block h-full w-full min-w-0 bg-field font-mono text-[12px] outline-2 -outline-offset-2 {mongo ? 'pr-[6.5rem] pl-2.5' : 'px-2.5'} {error ? 'outline-red-500' : 'outline-accent'}"
  title={error ?? undefined}
  aria-invalid={!!error}
  bind:this={input}
  oninput={() => oninput?.()}
  bind:value
  use:selectOnMount
  placeholder="NULL"
  autocapitalize="off"
  autocorrect="off"
  spellcheck="false"
  autocomplete="off"
  onkeydown={(e) => {
    if (e.key === 'Enter') oncommit(false);
    else if (e.key === 'Escape') { e.stopPropagation(); oncancel(); }
  }}
  onfocus={() => (picking = false)}
  onblur={(e) => {
    // Moving to the type list is not leaving the cell.
    if (mongo && (picking || e.relatedTarget === select)) return;
    oncommit(true);
  }}
/>
{#if mongo}
  <select
    class="absolute inset-y-0 right-0 w-24 border-l border-line bg-field px-1 font-sans text-[11px] text-muted outline-none"
    aria-label="BSON type"
    title="BSON type"
    bind:this={select}
    bind:value={type}
    onpointerdown={() => (picking = true)}
    onchange={() => input?.focus()}
    onkeydown={(e) => {
      if (e.key === 'Enter') oncommit(false);
      else if (e.key === 'Escape') { e.stopPropagation(); oncancel(); }
    }}
    onblur={(e) => {
      picking = false;
      if (e.relatedTarget !== input) oncommit(true);
    }}
    ondblclick={(e) => e.stopPropagation()}
  >
    {#each types as t (t)}<option value={t}>{t}</option>{/each}
  </select>
{/if}
