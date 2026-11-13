<script lang="ts">
  import { GitCommitHorizontal, LoaderCircle } from '@lucide/svelte';
  import { errorText } from '../api';
  import type { Tab } from '../tabs.svelte';
  import Modal from './Modal.svelte';

  /**
   * `end` commits or rolls back the transactions that are still open in `tabs`; it throws on the
   * first failure. `connName` belongs to the caller that owns the tabs.
   */
  let {
    tabs,
    action,
    connName,
    end,
    answer,
  }: {
    tabs: Tab[];
    action: string;
    connName: (connId: string) => string;
    end: (commit: boolean) => Promise<void>;
    answer: (settled: boolean) => void;
  } = $props();

  let busy = $state(false);
  let error = $state<string | null>(null);
  // Tabs already ended by a partly failed Commit or Rollback drop off the list.
  const left = $derived(tabs.filter((t) => t.status?.pending));
  // COMMIT on an aborted PG transaction only rolls back; offering it would mislead.
  const aborted = $derived(left.some((t) => t.status?.aborted));

  async function finish(commit: boolean) {
    busy = true;
    error = null;
    try {
      await end(commit);
      answer(true);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title="Uncommitted changes" role="alertdialog" {busy} {error} onclose={() => answer(false)}>
  {#snippet head()}<GitCommitHorizontal size={15} class="text-amber-600 dark:text-amber-400" />{/snippet}

  <p class="leading-relaxed text-muted">{action} ends the open transaction of {left.length === 1 ? 'this tab' : 'these tabs'}. Commit the changes or roll them back first.</p>
  <ul class="mt-3 divide-y divide-line rounded-lg border border-line">
    {#each left as t (t.id)}
      <li class="flex items-center gap-2 px-3 py-2">
        <span class="size-1.5 shrink-0 rounded-full bg-amber-500"></span>
        <b class="truncate font-medium">{t.title}</b>
        <span class="truncate text-[12.5px] text-muted">{connName(t.connId)}</span>
        {#if t.status?.aborted}<span class="tag ml-auto text-red-700 dark:text-red-400">aborted</span>{/if}
      </li>
    {/each}
  </ul>
  {#if aborted}
    <p class="mt-2.5 text-[12.5px] text-muted">A failed statement aborted a transaction; it can only be rolled back.</p>
  {/if}

  {#snippet footer()}
    <button class="btn btn-secondary" disabled={busy} onclick={() => answer(false)}>Cancel</button>
    <button class="btn btn-secondary" disabled={busy} onclick={() => finish(false)}>Rollback</button>
    <button class="btn btn-primary" disabled={busy || aborted} onclick={() => finish(true)}>
      {#if busy}<LoaderCircle size={13} class="animate-spin" />{/if} Commit
    </button>
  {/snippet}
</Modal>
