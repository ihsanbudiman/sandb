<script lang="ts">
  import { ShieldOff, Check } from '@lucide/svelte';
  import { envTag, canWrite, type Conn } from '../state.svelte';

  let { target = $bindable(null) }: { target?: Conn | null } = $props();

  function confirm() {
    if (target && canWrite(target)) target.aiWrite = true;
    target = null;
  }
</script>

{#if target}
  <div class="dialog-backdrop">
    <div class="dialog max-w-md">
      <header class="dialog-head">
        <ShieldOff size={15} class="text-amber-600 dark:text-amber-400" />
        {#if !canWrite(target)}
          <h2 class="flex-1">This connection is read-only</h2>
        {:else}
          <h2 class="flex-1">Let the assistant propose writes?</h2>
        {/if}
      </header>

      {#if !canWrite(target)}
        <!-- Every entry point lands here, the chat panel's write button included. -->
        <div class="dialog-body">
          <p class="leading-relaxed text-muted">
            <b class="font-medium text-ink">{target.name}</b> refuses all writes, so the assistant cannot propose any. Turn off read-only in the connection settings first.
          </p>
        </div>
        <footer class="dialog-foot">
          <button class="btn btn-secondary" onclick={() => (target = null)}>Close</button>
        </footer>
      {:else}
        <div class="dialog-body">
          <p class="leading-relaxed text-muted">
            {#if target.engine === 'mongo'}
              The assistant can then suggest writes such as insert, update, delete and drop on
            {:else}
              The assistant can then suggest INSERT, UPDATE, DELETE and DDL statements on
            {/if}
            <b class="font-medium text-ink">{target.name}</b>
            <span class="tag {envTag[target.env]}">{target.env}</span>
          </p>
          <ul class="mt-3 space-y-1.5 text-[12.5px]">
            <li class="flex gap-2"><Check size={14} class="mt-px shrink-0 text-emerald-600 dark:text-emerald-400" /> Every write opens a preview. Nothing runs until you approve it.</li>
            {#if target.engine === 'mongo'}
              <li class="flex gap-2"><Check size={14} class="mt-px shrink-0 text-emerald-600 dark:text-emerald-400" /> Each approved write is one mongosh statement and runs alone, not in a transaction.</li>
            {:else}
              <li class="flex gap-2"><Check size={14} class="mt-px shrink-0 text-emerald-600 dark:text-emerald-400" /> Approved statements run inside a transaction.</li>
            {/if}
            <li class="flex gap-2"><Check size={14} class="mt-px shrink-0 text-emerald-600 dark:text-emerald-400" /> Every statement is written to the audit log.</li>
          </ul>
        </div>
        <footer class="dialog-foot">
          <button class="btn btn-secondary" onclick={() => (target = null)}>Keep read-only</button>
          <button class="btn btn-primary" onclick={confirm}>Turn on write mode</button>
        </footer>
      {/if}
    </div>
  </div>
{/if}
