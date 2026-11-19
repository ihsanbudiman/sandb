<script lang="ts">
  import type { Snippet } from 'svelte';

  let { title, hint, icon, action, class: cls = '' }: {
    title: string;
    hint?: string;
    icon?: Snippet;
    action?: Snippet;
    class?: string;
  } = $props();
</script>

<div class="flex flex-col items-center gap-2 px-4 py-6 text-center {cls}">
  {#if icon}
    <span class="grid size-8 place-items-center rounded-lg bg-hover text-muted" aria-hidden="true">{@render icon()}</span>
  {/if}
  <div class="space-y-0.5">
    <p class="text-base font-semibold text-ink">{title}</p>
    {#if hint}<p class="text-sm text-muted">{hint}</p>{/if}
  </div>
  {#if action}<div class="mt-1">{@render action()}</div>{/if}
</div>
