<script lang="ts">
  import { untrack } from 'svelte';
  import { Braces } from '@lucide/svelte';
  import Modal from './Modal.svelte';

  /** One mongosh value to edit: a cell (`document` false) or a whole new document. Answers the trimmed text. */
  let { title, text, document, answer }: { title: string; text: string; document: boolean; answer: (text: string | null) => void } = $props();

  let draft = $state(untrack(() => text));
  let error = $state<string | null>(null);

  // The backend parses the text; this only catches an empty editor and a document that is not one `{ … }`.
  function save() {
    const t = draft.trim();
    if (!t) error = 'Enter a value';
    else if (document && !(t.startsWith('{') && t.endsWith('}'))) error = 'Enter one document: { … }';
    else answer(t);
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      save();
    }
  }
</script>

<svelte:window onkeydown={onkey} />

<Modal {title} width="max-w-xl" closable onclose={() => answer(null)}>
  {#snippet head()}<Braces size={15} class="text-faint" />{/snippet}

  <textarea
    class="input h-72 resize-y py-1.5 font-mono text-sm leading-relaxed"
    data-autofocus
    aria-label="Value in mongosh syntax"
    aria-invalid={!!error}
    bind:value={draft}
    oninput={() => (error = null)}
    autocapitalize="off"
    autocomplete="off"
    spellcheck="false"
  ></textarea>
  {#if error}
    <p class="mt-1.5 text-sm text-danger" role="alert">{error}</p>
  {:else}
    <p class="mt-1.5 text-sm text-faint">mongosh syntax: <span class="font-mono">ObjectId("…")</span>, <span class="font-mono">ISODate("…")</span>, <span class="font-mono">NumberLong("…")</span>, <span class="font-mono">NumberDecimal("…")</span>. A whole number is an int32; write <span class="font-mono">1.0</span> for a double.</p>
  {/if}

  {#snippet footer()}
    <button class="btn btn-sm btn-secondary" onclick={() => answer(null)}>Cancel</button>
    <button class="btn btn-sm btn-primary" onclick={save}>Apply</button>
  {/snippet}
</Modal>
