<script lang="ts">
  import { Trash, LoaderCircle } from '@lucide/svelte';
  import { errorText } from '../api';
  import type { Conn } from '../connections.svelte';
  import Modal from './Modal.svelte';
  import TargetBadge from './TargetBadge.svelte';
  import ProdAck, { ackBlocked } from './ProdAck.svelte';

  /**
   * `run` deletes the connection. It answers `false` when something else (unsaved edits, an open
   * transaction) stopped the delete, so the dialog stays open.
   */
  let { conn, run, answer }: { conn: Conn; run: () => Promise<boolean>; answer: (ok: boolean) => void } = $props();

  const prod = $derived(conn.env === 'production');
  let ack = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function remove() {
    busy = true;
    error = null;
    try {
      if (await run()) answer(true);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title="Delete this connection?" role="alertdialog" {busy} {error} errorLabel="Delete failed:" onclose={() => answer(false)}>
  {#snippet head()}<Trash size={15} class="text-red-600 dark:text-red-400" />{/snippet}

  <TargetBadge {conn} />
  <p class="mt-3 leading-relaxed text-muted">This removes the saved connection from sandb only. The database and its data are not changed. The audit log keeps its entries.</p>

  {#if prod}
    <ProdAck class="mt-3" bind:checked={ack}>
      I want to delete <b class="font-mono">{conn.name}</b>.
    </ProdAck>
  {/if}

  {#snippet footer()}
    <button class="btn btn-secondary" disabled={busy} onclick={() => answer(false)}>Cancel</button>
    <button class="btn bg-red-600 text-white enabled:hover:bg-red-500" disabled={ackBlocked(prod, ack, busy)} onclick={remove}>
      {#if busy}<LoaderCircle size={13} class="animate-spin" />{/if} Delete connection
    </button>
  {/snippet}
</Modal>
