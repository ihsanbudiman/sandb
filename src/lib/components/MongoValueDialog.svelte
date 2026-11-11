<script lang="ts">
  import { Braces, X } from '@lucide/svelte';

  /** One mongosh value to edit: a cell (`document` false) or a whole new document. `onsave` gets the trimmed text. */
  let { edit = $bindable(null) }: { edit?: { title: string; text: string; document: boolean; onsave: (text: string) => void } | null } = $props();

  let draft = $state('');
  let error = $state<string | null>(null);

  $effect(() => {
    draft = edit?.text ?? '';
    error = null;
  });

  // The backend parses the text; this only catches an empty editor and a document that is not one `{ … }`.
  function save() {
    if (!edit) return;
    const t = draft.trim();
    if (!t) error = 'Enter a value';
    else if (edit.document && !(t.startsWith('{') && t.endsWith('}'))) error = 'Enter one document: { … }';
    else {
      edit.onsave(t);
      edit = null;
    }
  }

  function onkey(e: KeyboardEvent) {
    if (!edit) return;
    if (e.key === 'Escape') edit = null;
    else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      save();
    }
  }

  function focus(el: HTMLTextAreaElement) {
    el.focus();
  }
</script>

<svelte:window onkeydown={onkey} />

{#if edit}
  <div class="dialog-backdrop">
    <div class="dialog max-w-xl" role="dialog" aria-labelledby="mongo-value-title">
      <header class="dialog-head">
        <Braces size={15} class="text-faint" />
        <h2 id="mongo-value-title" class="min-w-0 flex-1 truncate">{edit.title}</h2>
        <button class="btn btn-ghost -my-1 -mr-1.5 size-7 p-0" onclick={() => (edit = null)} aria-label="Close"><X size={15} /></button>
      </header>

      <div class="dialog-body">
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
      </div>

      <footer class="dialog-foot">
        <button class="btn btn-secondary" onclick={() => (edit = null)}>Cancel</button>
        <button class="btn btn-primary" onclick={save}>Apply</button>
      </footer>
    </div>
  </div>
{/if}
