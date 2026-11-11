<script lang="ts">
  import { ShieldAlert, TriangleAlert } from '@lucide/svelte';
  import { engines, envTag, type Conn } from '../state.svelte';
  import type { WriteInfo } from '../api';

  // The run waits on the answer: true runs the statements, false closes the dialog.
  let { ask = $bindable(null) }: { ask?: { info: WriteInfo; conn: Conn; resolve: (ok: boolean) => void } | null } = $props();

  function answer(ok: boolean) {
    ask?.resolve(ok);
    ask = null;
  }

  // Warnings that mean data loss or an unrecognized statement get the danger color.
  const DANGER = new Set(['update_without_where', 'delete_without_where', 'drop', 'rename', 'unknown_command']);
</script>

<svelte:window onkeydown={(e) => ask && e.key === 'Escape' && answer(false)} />

{#if ask}
  <div class="dialog-backdrop">
    <div class="dialog max-w-md" role="alertdialog" aria-labelledby="mongo-write-title">
      <header class="dialog-head">
        <ShieldAlert size={15} class="text-red-600 dark:text-red-400" />
        <h2 id="mongo-write-title" class="flex-1">Run write on MongoDB?</h2>
      </header>

      <div class="dialog-body">
        <p class="flex flex-wrap items-center gap-2 rounded-lg bg-sunken px-3 py-2">
          <span class="font-mono text-[10.5px] font-semibold {engines[ask.conn.engine].badge}">{engines[ask.conn.engine].short}</span>
          <b class="font-medium text-ink">{ask.conn.name}</b>
          <span class="tag {envTag[ask.conn.env]}">{ask.conn.env}</span>
        </p>
        <ul class="mt-3 space-y-1 font-mono text-[12px]">
          {#each ask.info.statements as s, i (i)}
            <li class="flex items-center gap-2">
              <span aria-hidden="true" class="w-4 shrink-0 text-right text-faint">{i + 1}</span>
              <span class="min-w-0 truncate"><b class="font-semibold text-ink">{s.kind}</b>{#if s.database}<span class="text-muted"> · {s.database}</span>{/if}{#if s.table}<span class="text-muted">{s.database ? '.' : ' · '}{s.table}</span>{/if}</span>
            </li>
          {/each}
        </ul>
        {#if ask.info.warnings.length}
          <ul class="mt-2.5 space-y-1 text-[12px]">
            {#each ask.info.warnings as w, i (i)}
              <li class="flex items-start gap-2 {DANGER.has(w.code) ? 'text-red-700 dark:text-red-400' : 'text-muted'}">
                <TriangleAlert size={12} class="mt-0.5 shrink-0" />
                <span class="min-w-0 break-words">{w.message}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </div>

      <footer class="dialog-foot">
        <button class="btn btn-secondary" onclick={() => answer(false)}>Cancel</button>
        <button class="btn bg-red-600 text-white enabled:hover:bg-red-500" onclick={() => answer(true)}>Run</button>
      </footer>
    </div>
  </div>
{/if}
