<script lang="ts">
  import { asks } from '../ask.svelte';
</script>

{#each asks.list as entry (entry.id)}
  {@const Dialog = entry.dialog}
  <Dialog {...entry.props} />
{/each}
