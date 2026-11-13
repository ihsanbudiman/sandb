<script lang="ts" module>
  import type { DocumentExportFormat, EjsonFlavor } from '../api';

  const formats: { value: DocumentExportFormat; label: string }[] = [
    { value: 'csv', label: 'CSV' },
    { value: 'json', label: 'JSON array' },
    { value: 'ndjson', label: 'NDJSON' },
  ];
  const ejsonModes: { value: EjsonFlavor; label: string }[] = [
    { value: 'relaxed', label: 'Relaxed' },
    { value: 'canonical', label: 'Canonical' },
  ];
</script>

<script lang="ts">
  import { Download } from '@lucide/svelte';
  import { errorText, type ExportResult } from '../api';
  import { exportDocumentsPrefs } from '../dialogPrefs.svelte';
  import Modal from './Modal.svelte';

  /**
   * `run` writes the file; it throws so the dialog keeps the error. A closed save dialog keeps it
   * open too. The answer carries the format for the caller's notice.
   */
  let {
    run,
    answer,
  }: {
    run: (format: DocumentExportFormat, ejson: EjsonFlavor) => Promise<ExportResult>;
    answer: (result: { format: DocumentExportFormat; out: ExportResult } | null) => void;
  } = $props();

  let format = $state(exportDocumentsPrefs.format);
  let ejson = $state(exportDocumentsPrefs.ejson);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function submit() {
    if (busy) return;
    Object.assign(exportDocumentsPrefs, { format, ejson });
    busy = true;
    error = null;
    try {
      const out = await run(format, ejson);
      if (!out.cancelled) answer({ format, out });
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title="Export documents" closable {busy} {error} onclose={() => answer(null)}>
  {#snippet head()}<Download size={15} class="text-faint" />{/snippet}

  <div class="space-y-4">
    <div class="space-y-1.5">
      <span class="label">Format</span>
      <div class="seg w-fit" role="radiogroup" aria-label="Format">
        {#each formats as f (f.value)}
          <button type="button" class="seg-item {format === f.value ? 'seg-on' : ''}" role="radio" aria-checked={format === f.value} onclick={() => (format = f.value)}>{f.label}</button>
        {/each}
      </div>
      <p class="text-[12px] leading-relaxed text-faint">
        {#if format === 'csv'}
          One row for each field path of the documents; arrays and nested documents become JSON text.
        {:else if format === 'json'}
          One JSON array that holds every document.
        {:else}
          One compact document on each line.
        {/if}
      </p>
    </div>

    {#if format !== 'csv'}
      <div class="space-y-1.5">
        <span class="label">Extended JSON</span>
        <div class="seg w-fit" role="radiogroup" aria-label="Extended JSON">
          {#each ejsonModes as o (o.value)}
            <button type="button" class="seg-item {ejson === o.value ? 'seg-on' : ''}" role="radio" aria-checked={ejson === o.value} onclick={() => (ejson = o.value)}>{o.label}</button>
          {/each}
        </div>
        <p class="text-[12px] leading-relaxed text-faint">Canonical keeps every type, for example int64 and decimal; relaxed is easier to read.</p>
      </div>
    {/if}
  </div>

  {#snippet footer()}
    <button class="btn btn-secondary" onclick={() => answer(null)} disabled={busy}>Cancel</button>
    <button class="btn btn-primary" disabled={busy} onclick={submit}>Export...</button>
  {/snippet}
</Modal>
