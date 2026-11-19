<script lang="ts">
  import { ShieldAlert, TriangleAlert } from '@lucide/svelte';
  import type { WarningCode, WriteInfo } from '../api';
  import { engines, type Conn } from '../connections.svelte';
  import Modal from './Modal.svelte';
  import TargetBadge from './TargetBadge.svelte';

  let { info, conn, answer }: { info: WriteInfo; conn: Conn; answer: (ok: boolean) => void } = $props();

  // Red for data loss, server changes or an unrecognized statement.
  const DANGER: Record<WarningCode, boolean> = {
    update_without_where: true,
    delete_without_where: true,
    drop: true,
    rename: true,
    unknown_command: true,
    flush: true,
    server_admin: true,
    truncate: false,
    alter_drop: false,
    multiple_statements: false,
    unparsable: false,
    executable_comment: false,
    implicit_commit: false,
  };
</script>

<Modal title={`Run write on ${engines[conn.engine].label}?`} role="alertdialog" onclose={() => answer(false)}>
  {#snippet head()}<ShieldAlert size={15} class="text-danger" />{/snippet}

  <TargetBadge {conn} />
  <ul class="mt-3 space-y-1 font-mono text-sm">
    {#each info.statements as s, i (i)}
      <li class="flex items-center gap-2">
        <span aria-hidden="true" class="w-4 shrink-0 text-right text-2xs text-faint">{i + 1}</span>
        <span class="min-w-0 truncate"><b class="font-semibold text-ink">{s.kind}</b>{#if s.database}<span class="text-muted"> · {s.database}</span>{/if}{#if s.table}<span class="text-muted">{s.database ? '.' : ' · '}{s.table}</span>{/if}</span>
      </li>
    {/each}
  </ul>
  {#if info.warnings.length}
    <ul class="mt-2.5 space-y-1 text-sm">
      {#each info.warnings as w, i (i)}
        <li class="flex items-start gap-2 {DANGER[w.code] ? 'text-danger' : 'text-muted'}">
          <TriangleAlert size={13} class="mt-0.5 shrink-0" />
          <span class="min-w-0 break-words">{w.message}</span>
        </li>
      {/each}
    </ul>
  {/if}

  {#snippet footer()}
    <button class="btn btn-sm btn-secondary" data-autofocus onclick={() => answer(false)}>Cancel</button>
    <button class="btn btn-sm btn-danger" onclick={() => answer(true)}>Run</button>
  {/snippet}
</Modal>
