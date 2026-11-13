<script lang="ts">
  import { Sparkles } from '@lucide/svelte';
  import Markdown from './Markdown.svelte';
  import Modal from './Modal.svelte';
  import { appVersion, closeWhatsNew, whatsNew } from '../updates.svelte';
</script>

{#if whatsNew.releases.length}
  <Modal title="What's new in sandb {appVersion}" width="max-w-lg" class="flex max-h-[80vh] flex-col" bodyClass="min-h-0 space-y-4 overflow-y-auto" onclose={closeWhatsNew}>
    {#snippet head()}<Sparkles size={15} class="text-accent" />{/snippet}

    {#each whatsNew.releases as r (r.version)}
      <section>
        <h3 class="text-[12.5px] font-semibold text-ink">{r.version} <span class="font-normal text-faint">{r.date}</span></h3>
        <div class="text-[12.5px] text-muted"><Markdown text={r.notes} /></div>
      </section>
    {/each}

    {#snippet footer()}
      <button class="btn btn-primary" onclick={closeWhatsNew}>Got it</button>
    {/snippet}
  </Modal>
{/if}
