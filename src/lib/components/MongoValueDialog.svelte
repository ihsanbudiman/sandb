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

  const focus = (el: HTMLTextAreaElement) => el.focus();
</script>

<svelte:window onkeydown={onkey} />

<Modal {title} width="max-w-xl" closable onclose={() => answer(null)}>
  {#snippet head()}<Braces size={15} class="text-faint" />{/snippet}

  <textarea
    class="input h-72 w-full resize-y py-1.5 font-mono text-[12px] leading-relaxed {error ? 'outline-red-500' : ''}"
    aria-label="Value in mongosh syntax"
    aria-invalid={!!error}
    bind:value={draft}
    oninput={() => (error = null)}
    use:focus
    autocapitalize="off"
    autocomplete="off"
    spellcheck="false"
  ></textarea>
  {#if error}
    <p class="mt-1.5 text-[12px] text-red-700 dark:text-red-400" role="alert">{error}</p>
  {:else}
    <p class="mt-1.5 text-[12px] text-faint">mongosh syntax: <span class="font-mono">ObjectId("…")</span>, <span class="font-mono">ISODate("…")</span>, <span class="font-mono">NumberLong("…")</span>, <span class="font-mono">NumberDecimal("…")</span>. A whole number is an int32; write <span class="font-mono">1.0</span> for a double.</p>
  {/if}

  {#snippet footer()}
    <button class="btn btn-secondary" onclick={() => answer(null)}>Cancel</button>
    <button class="btn btn-primary" onclick={save}>Apply</button>
  {/snippet}
</Modal>
