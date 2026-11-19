<script lang="ts">
  import { ShieldCheck } from '@lucide/svelte';
  import { envDot, envTag, environmentLabel, type Conn } from '../connections.svelte';

  let { conn, compact = false }: { conn: Pick<Conn, 'env' | 'envLabel'>; compact?: boolean } = $props();
  const label = $derived(environmentLabel(conn));
  const protectedWrites = $derived(conn.env === 'production');
</script>

{#if compact && !conn.envLabel && !protectedWrites}
  <span class="size-1.5 shrink-0 rounded-full {envDot[conn.env]}" title={label}></span><span class="sr-only">{label}</span>
{:else}
  <span class="tag max-w-28 shrink-0 {envTag[conn.env]}" title={protectedWrites ? label + ' · Protected writes' : label}>
    {#if protectedWrites}<ShieldCheck size={12} class="shrink-0" aria-hidden="true" /><span class="sr-only">Protected writes: </span>{/if}
    <span class="truncate">{label}</span>
  </span>
{/if}
