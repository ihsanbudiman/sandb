<script lang="ts">
  import { untrack } from 'svelte';
  import { BookmarkPlus } from '@lucide/svelte';
  import { engines, type Conn } from '../connections.svelte';
  import { KEYS } from '../keys';
  import Modal from './Modal.svelte';

  /** The name dialog of a query: `conn` is the connection it is saved for, when the tab has one. */
  let {
    name: initial,
    copy,
    conn,
    database,
    lines,
    answer,
  }: {
    name: string;
    copy: boolean;
    conn: Conn | undefined;
    database: string;
    lines: number;
    answer: (name: string | null) => void;
  } = $props();

  let name = $state(untrack(() => initial));

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (name.trim()) answer(name.trim());
  }

  // The name is set before the dialog mounts, so it is ready to select.
  const select = (el: HTMLInputElement) => {
    el.focus();
    el.select();
  };
</script>

<Modal title={copy ? 'Save as new query' : 'Save query'} width="max-w-sm" onsubmit={submit} onclose={() => answer(null)}>
  {#snippet head()}<BookmarkPlus size={15} class="text-accent" />{/snippet}

  <div class="space-y-3">
    <label class="block space-y-1.5">
      <span class="label">Name</span>
      <input class="input" bind:value={name} use:select placeholder="e.g. Daily paid orders" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
    </label>
    <div class="space-y-1.5">
      <span class="label">Saved for</span>
      <p class="flex items-center gap-2 rounded-lg bg-sunken px-3 py-2 text-[12.5px]">
        {#if conn}<span class="font-mono text-[10.5px] font-semibold {engines[conn.engine].badge}">{engines[conn.engine].short}</span>{/if}
        <span class="min-w-0 truncate font-medium text-ink">{conn?.name ?? 'No connection'}</span>
        {#if database}<span class="text-faint">/</span><span class="min-w-0 truncate text-muted">{database}</span>{/if}
        <span class="ml-auto shrink-0 text-[11.5px] text-faint tabular-nums">{lines} {lines === 1 ? 'line' : 'lines'}</span>
      </p>
    </div>
    <p class="text-[12px] leading-relaxed text-faint">Opening the query later selects this connection and database again. After this, <kbd class="kbd">{KEYS.save.hint}</kbd> saves your changes without this dialog.</p>
  </div>

  {#snippet footer()}
    <button type="button" class="btn btn-secondary" onclick={() => answer(null)}>Cancel</button>
    <button type="submit" class="btn btn-primary" disabled={!name.trim()}>Save <kbd class="ml-0.5 font-sans text-[11px] font-normal opacity-80" aria-hidden="true">↵</kbd></button>
  {/snippet}
</Modal>
