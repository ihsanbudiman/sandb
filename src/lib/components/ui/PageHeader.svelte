<script lang="ts">
  import type { Snippet } from 'svelte';
  import { ArrowLeft } from '@lucide/svelte';

  let { title, subtitle, onback, backLabel = 'Back to query', actions, class: cls = '' }: {
    title: string;
    subtitle?: string;
    onback: () => void;
    backLabel?: string;
    actions?: Snippet;
    class?: string;
  } = $props();
</script>

<header class="flex h-page-header shrink-0 items-center gap-2.5 px-4 {cls}">
  <button type="button" class="btn btn-ghost -ml-1.5 size-7 shrink-0 p-0" aria-label={backLabel} title={backLabel} onclick={onback}>
    <ArrowLeft size={15} />
  </button>
  <div class="flex min-w-0 flex-1 items-baseline gap-2.5">
    <h1 class="shrink-0 text-lg font-semibold">{title}</h1>
    {#if subtitle}<span class="truncate text-sm text-faint">{subtitle}</span>{/if}
  </div>
  {#if actions}<div class="flex shrink-0 items-center gap-2">{@render actions()}</div>{/if}
</header>
