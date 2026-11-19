<script lang="ts">
  import { TriangleAlert } from '@lucide/svelte';
  import type { Asked, ConfirmOptions } from '../../ask.svelte';
  import Modal from '../Modal.svelte';

  let { title, body, confirmLabel = 'Confirm', destructive = false, answer }: ConfirmOptions & Asked<boolean> = $props();
  const bodyId = $props.id();
</script>

<Modal {title} role={destructive ? 'alertdialog' : 'dialog'} describedBy={bodyId} onclose={() => answer(false)}>
  {#snippet head()}
    {#if destructive}<TriangleAlert size={15} class="shrink-0 text-danger" />{/if}
  {/snippet}

  <p id={bodyId} class="break-words whitespace-pre-line text-base text-muted">{body}</p>

  {#snippet footer()}
    <button type="button" class="btn btn-secondary" data-autofocus={destructive ? '' : undefined} onclick={() => answer(false)}>Cancel</button>
    <button type="button" class="btn {destructive ? 'btn-danger' : 'btn-primary'}" data-autofocus={destructive ? undefined : ''} onclick={() => answer(true)}>{confirmLabel}</button>
  {/snippet}
</Modal>
