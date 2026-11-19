<script lang="ts">
  import EnvironmentBadge from './EnvironmentBadge.svelte';
  import { ShieldOff, Check } from '@lucide/svelte';
  import { canWrite, type Conn } from '../connections.svelte';
  import Modal from './Modal.svelte';

  /** The connection the assistant would get writes for; no stored `Conn` is needed, only what the texts name. */
  let { conn, answer }: { conn: Pick<Conn, 'name' | 'engine' | 'env' | 'envLabel' | 'readOnly'>; answer: (ok: boolean) => void } = $props();
  const readOnly = $derived(!canWrite(conn));
</script>

<Modal
  title={readOnly ? 'This connection is read-only' : 'Let the assistant propose writes?'}
  describedBy="safeguard-help"
  onclose={() => answer(false)}
>
  {#snippet head()}<ShieldOff size={15} class="text-warn" />{/snippet}

  {#if readOnly}
    <!-- Every entry point lands here, the chat panel's write button included. -->
    <p id="safeguard-help" class="leading-relaxed text-muted">
      <b class="font-medium text-ink">{conn.name}</b> refuses all writes, so the assistant cannot propose any. Turn off read-only in the connection settings first.
    </p>
  {:else}
    <p id="safeguard-help" class="leading-relaxed text-muted">
      {#if conn.engine === 'mongo'}
        The assistant can then suggest writes such as insert, update, delete and drop on
      {:else}
        The assistant can then suggest INSERT, UPDATE, DELETE and DDL statements on
      {/if}
      <b class="font-medium text-ink">{conn.name}</b>
      <EnvironmentBadge conn={conn} />
    </p>
    <ul class="mt-3 space-y-1.5 text-sm">
      <li class="flex gap-2"><Check size={13} class="mt-px shrink-0 text-ok" /> Every write opens a preview. Nothing runs until you approve it.</li>
      {#if conn.engine === 'mongo'}
        <li class="flex gap-2"><Check size={13} class="mt-px shrink-0 text-ok" /> Each approved write is one mongosh statement and runs alone, not in a transaction.</li>
      {:else if conn.engine === 'mysql'}
        <li class="flex gap-2"><Check size={13} class="mt-px shrink-0 text-ok" /> Approved statements run inside one transaction, except statements that commit on their own.</li>
      {:else}
        <li class="flex gap-2"><Check size={13} class="mt-px shrink-0 text-ok" /> Approved statements run inside one transaction.</li>
      {/if}
      <li class="flex gap-2"><Check size={13} class="mt-px shrink-0 text-ok" /> Every statement is written to the audit log.</li>
    </ul>
  {/if}

  {#snippet footer()}
    {#if readOnly}
      <button class="btn btn-sm btn-secondary" data-autofocus onclick={() => answer(false)}>Close</button>
    {:else}
      <button class="btn btn-sm btn-secondary" data-autofocus onclick={() => answer(false)}>Keep read-only</button>
      <button class="btn btn-sm btn-primary" onclick={() => answer(true)}>Turn on write mode</button>
    {/if}
  {/snippet}
</Modal>
