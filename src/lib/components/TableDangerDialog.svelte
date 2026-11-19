<script lang="ts">
  import { untrack } from 'svelte';
  import { Trash, Eraser, LoaderCircle } from '@lucide/svelte';
  import { errorText } from '../api';
  import { dangerSql, displayName, kindLabel, type Danger } from '../tableActions';
  import Modal from './Modal.svelte';
  import TargetBadge from './TargetBadge.svelte';
  import ProdAck, { ackBlocked } from './ProdAck.svelte';

  /** `run` executes the statement the preview shows; it throws so the dialog can keep the error. */
  let { danger, run, answer }: { danger: Danger; run: (sql: string) => Promise<void>; answer: (ok: boolean) => void } = $props();

  const d = untrack(() => danger);
  const drop = d.action === 'drop';
  const what = kindLabel[d.t.kind];
  const sql = dangerSql(d.c.engine, d.t, d.action);
  const prod = d.c.env === 'production';

  let ack = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function go() {
    busy = true;
    error = null;
    try {
      await run(sql);
      answer(true);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal
  title={drop ? `Drop this ${what}?` : 'Truncate this table?'}
  role="alertdialog"
  describedBy="table-danger-help"
  {busy}
  {error}
  errorLabel={drop ? 'Drop failed:' : 'Truncate failed:'}
  onclose={() => answer(false)}
>
  {#snippet head()}
    {#if drop}<Trash size={15} class="text-danger" />{:else}<Eraser size={15} class="text-danger" />{/if}
  {/snippet}

  <TargetBadge conn={d.c} target={d.t} />
  <p id="table-danger-help" class="mt-3 leading-relaxed text-muted">
    {drop ? `This removes the ${what} and all its data from the database.` : 'This deletes every row of the table.'}
    The statement runs immediately and commits. You cannot undo it.
  </p>
  <pre class="mt-3 overflow-x-auto rounded-lg bg-sunken px-3 py-2 font-mono text-sm text-ink">{sql}</pre>

  {#if prod}
    <ProdAck class="mt-3" bind:checked={ack}>
      I want to {d.action} <b class="font-mono">{displayName(d.t)}</b>.
    </ProdAck>
  {/if}

  {#snippet footer()}
    <button class="btn btn-sm btn-secondary" data-autofocus disabled={busy} onclick={() => answer(false)}>Cancel</button>
    <button class="btn btn-sm btn-danger" disabled={ackBlocked(prod, ack, busy)} onclick={go}>
      {#if busy}<LoaderCircle size={12} class="motion-safe:animate-spin" />{/if} {drop ? `Drop ${what}` : 'Truncate table'}
    </button>
  {/snippet}
</Modal>
