<script lang="ts" module>
  import type { DocumentFormat, EjsonMode } from '../api';

  // The format choices carry over to the next export in this session.
  const last = { format: 'json' as DocumentFormat, ejson: 'relaxed' as EjsonMode };
  const formats: { value: DocumentFormat; label: string }[] = [
    { value: 'csv', label: 'CSV' },
    { value: 'json', label: 'JSON array' },
    { value: 'ndjson', label: 'NDJSON' },
  ];
  const ejsonModes: { value: EjsonMode; label: string }[] = [
    { value: 'relaxed', label: 'Relaxed' },
    { value: 'canonical', label: 'Canonical' },
  ];
</script>

<script lang="ts">
  import { X, Download } from '@lucide/svelte';
  import { ApiError } from '../api';

  /** `onexport` throws to keep the dialog open with the error; it closes on success. A closed save dialog keeps it open. */
  let { onexport, onclose }: { onexport: (format: DocumentFormat, ejson: EjsonMode) => Promise<void>; onclose: () => void } = $props();

  let format = $state(last.format);
  let ejson = $state(last.ejson);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function submit() {
    if (busy) return;
    Object.assign(last, { format, ejson });
    busy = true;
    error = null;
    try {
      await onexport(format, ejson);
    } catch (e) {
      error = ApiError.from(e).message;
    } finally {
      busy = false;
    }
  }
</script>

<svelte:window onkeydown={(e) => { if (e.key === 'Escape' && !busy) onclose(); }} />

<div class="dialog-backdrop">
  <div class="dialog max-w-md">
    <header class="dialog-head">
      <Download size={15} class="text-faint" />
      <h2 class="flex-1">Export documents</h2>
      <button class="btn btn-ghost -my-1 -mr-1.5 size-7 p-0" onclick={onclose} disabled={busy} aria-label="Close"><X size={15} /></button>
    </header>

    <div class="dialog-body space-y-4">
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

    <footer class="dialog-foot">
      {#if error}<span class="mr-auto min-w-0 truncate text-[12px] text-red-700 dark:text-red-400" title={error}>{error}</span>{/if}
      <button class="btn btn-secondary" onclick={onclose} disabled={busy}>Cancel</button>
      <button class="btn btn-primary" disabled={busy} onclick={submit}>Export...</button>
    </footer>
  </div>
</div>
