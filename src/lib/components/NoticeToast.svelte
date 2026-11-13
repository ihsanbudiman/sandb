<script lang="ts">
  import { X } from '@lucide/svelte';
  import { dismissNotice, notice } from '../notice.svelte';
</script>

{#if notice.current}
  {@const n = notice.current}
  <div class="popover fixed bottom-3 left-3 z-50 flex max-w-md items-start gap-2 px-3 py-2 text-[12.5px] {n.ok ? 'text-ink' : 'text-red-700 dark:text-red-300'}" role={n.ok ? 'status' : 'alert'}>
    <span class="min-w-0 flex-1 break-words">{n.text}</span>
    {#if n.action}
      {@const a = n.action}
      <!-- Run first: `a` reads the notice, which is gone once cleared. -->
      <button class="-my-0.5 h-6 shrink-0 rounded px-1.5 text-[12.5px] font-medium text-accent hover:bg-hover" onclick={() => { a.run(); dismissNotice(); }}>{a.label}</button>
    {/if}
    <button class="grid size-5 shrink-0 place-items-center rounded text-faint hover:bg-hover hover:text-ink" aria-label="Dismiss" onclick={dismissNotice}><X size={12} /></button>
  </div>
{/if}
