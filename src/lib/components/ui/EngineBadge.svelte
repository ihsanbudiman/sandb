<script lang="ts">
  import { engines, type Engine } from '../../connections.svelte';

  let { engine, class: cls = '' }: { engine: Engine; class?: string } = $props();
  const badge = $derived(engines[engine]);
</script>

<span class="shrink-0 font-mono text-2xs font-semibold {badge.badge} {cls}" title={badge.label}>{badge.short}</span>
