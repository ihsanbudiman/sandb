<script lang="ts">
  import { selectOnMount } from '../actions';
  import Select from './ui/Select.svelte';

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
  let select = $state<HTMLButtonElement>();
  const options = $derived(types.map((value) => ({ value, label: value })));

  function leave(e: FocusEvent & { currentTarget: HTMLDivElement }) {
    // Both the trigger and the listbox are inside the cell editor's focus boundary.
    if (e.relatedTarget instanceof Node && e.currentTarget.contains(e.relatedTarget)) return;
    oncommit(true);
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions (Escape cancels the focused inline editor unless the listbox handled it.) -->
<div
  class="absolute inset-0 flex min-w-0 items-center bg-field"
  role="group"
  aria-label="Cell editor"
  onfocusout={leave}
  onkeydown={(e) => {
    if (e.key === 'Escape') { e.stopPropagation(); oncancel(); }
  }}
>
  <input
    class="input input-sm h-full min-w-0 flex-1 rounded-none font-mono select-text ring-inset focus:ring-2 focus:ring-accent/60"
    aria-label="Cell value"
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
    }}
  />
  {#if mongo}
    <Select
      class="h-full w-24 shrink-0 rounded-none px-1.5 text-xs"
      size="sm"
      aria-label="BSON type"
      title="BSON type"
      bind:trigger={select}
      bind:value={type}
      {options}
      onpointerdown={() => select?.focus({ preventScroll: true })}
      onchange={() => input?.focus()}
      ondblclick={(e) => e.stopPropagation()}
    />
  {/if}
</div>
