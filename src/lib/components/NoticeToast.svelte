<script lang="ts">
  import { X } from '@lucide/svelte';
  import { fly } from 'svelte/transition';
  import { dismissNotice, notice } from '../notice.svelte';
</script>

<div class="pointer-events-none absolute right-3 bottom-3 left-3 z-toast flex flex-col items-end gap-2">
  {#each notice.list as n (n.id)}
    <div class="popover pointer-events-auto flex max-w-md items-start gap-2 px-3 py-2 text-sm {n.ok ? 'text-ink' : 'text-danger'}" role={n.ok ? 'status' : 'alert'} in:fly={{ x: 12, duration: matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : 150 }}>
      <span class="min-w-0 flex-1 break-words">{n.text}</span>
      {#if n.action}
        {@const a = n.action}
        <button class="btn btn-sm btn-ghost -my-0.5 shrink-0 text-accent" onclick={() => { dismissNotice(n.id); a.run(); }}>{a.label}</button>
      {/if}
      <button class="btn btn-ghost size-5 shrink-0 p-0" aria-label="Dismiss" onclick={() => dismissNotice(n.id)}><X size={12} /></button>
    </div>
  {/each}
</div>
