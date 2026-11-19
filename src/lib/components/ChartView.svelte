<script lang="ts">
  import { echarts, chartOption, themeColors } from '../charts';
  import type { ChartPart } from '../chatModel';
  import { ui } from '../state.svelte';

  let { part }: { part: ChartPart } = $props();
  let el = $state<HTMLDivElement>();

  $effect(() => {
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'svg' });
    const resize = new ResizeObserver(() => chart.resize());
    resize.observe(el);
    $effect(() => {
      chart.setOption(chartOption(part, themeColors(ui.dark)), true);
    });
    return () => {
      resize.disconnect();
      chart.dispose();
    };
  });
</script>

<figure class="rounded-xl border border-line bg-sunken px-6 pt-6 pb-4 dark:bg-canvas">
  {#if part.spec.title || part.truncated}
    <figcaption class="mb-4">
      {#if part.spec.title}<p class="text-lg font-semibold whitespace-pre-wrap [overflow-wrap:anywhere]">{part.spec.title}</p>{/if}
      {#if part.truncated}<p class="mt-1 text-sm text-warn">The query has more rows. The chart shows the first {part.rows.length}.</p>{/if}
    </figcaption>
  {/if}
  <div bind:this={el} class="h-72 w-full" role="img" aria-label={part.spec.title ? `Chart: ${part.spec.title}` : 'Chart'}></div>
</figure>
