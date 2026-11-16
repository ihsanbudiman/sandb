<script lang="ts">
  import { untrack } from 'svelte';
  import { Variable } from '@lucide/svelte';
  import Modal from './Modal.svelte';

  /** Asks a value for each `$1`, `$2`… of a query before it runs; `values` are the last ones typed. */
  let { names, values: initial, answer }: { names: string[]; values: Record<string, string>; answer: (values: Record<string, string> | null) => void } = $props();

  let values = $state(untrack(() => Object.fromEntries(names.map((n) => [n, initial[n] ?? '']))));
  const ready = $derived(names.every((n) => values[n].trim()));

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (ready) answer(Object.fromEntries(names.map((n) => [n, values[n].trim()])));
  }

  // The first field takes the focus, with the remembered value selected.
  const select = (el: HTMLInputElement, first: boolean) => {
    if (!first) return;
    el.focus();
    el.select();
  };
</script>

<Modal title="Query parameters" width="max-w-sm" onsubmit={submit} onclose={() => answer(null)}>
  {#snippet head()}<Variable size={15} class="text-accent" />{/snippet}

  <div class="space-y-3">
    <div class="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2">
      {#each names as n, i (n)}
        <label class="font-mono text-[12.5px] text-muted" for="query-param-{i}">{n}</label>
        <input id="query-param-{i}" class="input font-mono" bind:value={values[n]} use:select={i === 0} placeholder="e.g. 42 or 'paid'" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
      {/each}
    </div>
    <p class="text-[12px] leading-relaxed text-faint">Each value goes into the query as written. Put text in quotes, for example <code class="font-mono">'paid'</code>. Use <code class="font-mono">NULL</code> for no value.</p>
  </div>

  {#snippet footer()}
    <button type="button" class="btn btn-secondary" onclick={() => answer(null)}>Cancel</button>
    <button type="submit" class="btn btn-primary" disabled={!ready}>Run <kbd class="ml-0.5 font-sans text-[11px] font-normal opacity-80" aria-hidden="true">↵</kbd></button>
  {/snippet}
</Modal>
