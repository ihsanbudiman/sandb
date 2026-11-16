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
  <div bind:this={el} class="h-72 w-full" role="img" aria-label={part.spec.title ? `Chart: ${part.spec.title}` : 'Chart'}></div>
  {#if part.truncated}
    <figcaption class="pt-1 text-[12px] text-amber-700 dark:text-amber-400">The query has more rows. The chart shows the first {part.rows.length}.</figcaption>
  {/if}
</figure>
