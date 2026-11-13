<script lang="ts">
  import { untrack } from 'svelte';
  import { FileCode } from '@lucide/svelte';
  import { errorText, type Engine, type ExportResult, type SqlOptions } from '../api';
  import { EXPORT_CONFLICT, MAX_ROWS_PER_STATEMENT } from '../api/generated/constants';
  import { exportSqlPrefs } from '../dialogPrefs.svelte';
  import Modal from './Modal.svelte';

  /** `key`: default conflict key (the primary key when known), as result column numbers. */
  let {
    columns,
    types = [],
    engine,
    schema: schema0,
    table: table0,
    key: key0,
    run,
    answer,
  }: {
    columns: string[];
    types?: string[];
    engine: Engine;
    schema: string | null;
    table: string;
    key: number[];
    run: (schema: string | null, table: string, options: SqlOptions) => Promise<ExportResult>;
    answer: (result: ExportResult | null) => void;
  } = $props();

  let schema = $state(untrack(() => schema0 ?? ''));
  let table = $state(untrack(() => table0));
  let picked = $state<boolean[]>(untrack(() => columns.map(() => true)));
  let key = $state<boolean[]>(untrack(() => columns.map((_, i) => key0.includes(i))));
  let mode = $state(exportSqlPrefs.mode);
  let rows = $state(exportSqlPrefs.rowsPerStatement);
  let transaction = $state(exportSqlPrefs.transaction);
  let header = $state(exportSqlPrefs.header);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const chosen = $derived(picked.flatMap((p, i) => (p ? [i] : [])));
  const keyCols = $derived(key.flatMap((k, i) => (k ? [i] : [])));
  const schemaLabel = $derived(engine === 'mysql' ? 'Database' : 'Schema');
  const problem = $derived.by(() => {
    if (!table.trim()) return 'Name the target table';
    if (!chosen.length) return 'Choose at least one column';
    if (!Number.isInteger(rows) || rows < 1 || rows > MAX_ROWS_PER_STATEMENT) return `Rows per statement must be from 1 to ${MAX_ROWS_PER_STATEMENT}`;
    if (mode === 'upsert') {
      if (engine !== 'mysql' && !keyCols.length) return 'Choose the key columns that find an existing row';
      if (keyCols.some((k) => !picked[k])) return 'Export every key column';
    }
    return null;
  });
  // The clauses the backend writes for this engine; the grid offers this dialog only on a SQL result.
  const conflict = $derived(engine === 'mongo' ? null : EXPORT_CONFLICT[engine]);
  const skipText = $derived(conflict?.skip ?? '');
  const upsertText = $derived(conflict?.upsert ?? '');

  function setAll(on: boolean) {
    picked = columns.map(() => on);
  }

  async function submit() {
    if (problem || busy) return;
    Object.assign(exportSqlPrefs, { mode, rowsPerStatement: rows, transaction, header });
    busy = true;
    error = null;
    try {
      const out = await run(schema.trim() || null, table.trim(), {
        columns: chosen.length === columns.length ? null : chosen,
        mode,
        key: mode === 'upsert' ? keyCols : [],
        rowsPerStatement: rows,
        transaction,
        header,
      });
      // The save dialog was closed: nothing was written, so this dialog stays open.
      if (!out.cancelled) answer(out);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title="Export as SQL" width="max-w-xl" closable {busy} {error} onclose={() => answer(null)}>
  {#snippet head()}<FileCode size={15} class="text-faint" />{/snippet}

  <div class="space-y-4">
    <div class="grid grid-cols-[2fr_3fr] gap-2">
      <label class="block space-y-1.5"><span class="label">{schemaLabel} <span class="font-normal text-faint">· empty to omit</span></span><input class="input font-mono" bind:value={schema} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
      <label class="block space-y-1.5"><span class="label">Target table</span><input class="input font-mono" bind:value={table} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
    </div>

    <div class="space-y-1.5">
      <div class="flex items-center gap-2">
        <span class="label flex-1">Columns <span class="font-normal text-faint">· {chosen.length} of {columns.length}</span></span>
        <button class="btn btn-ghost h-6 px-1.5 text-[12px]" onclick={() => setAll(true)}>All</button>
        <button class="btn btn-ghost h-6 px-1.5 text-[12px]" onclick={() => setAll(false)}>None</button>
      </div>
      <div class="max-h-52 overflow-auto rounded-lg border border-line">
        <table class="w-full text-[12.5px]">
          <thead class="sticky top-0 bg-sunken text-left text-[11px] text-faint">
            <tr><th class="w-8 px-2 py-1"></th><th class="px-2 py-1 font-medium">Column</th><th class="px-2 py-1 font-medium">Type</th>{#if mode === 'upsert'}<th class="w-12 px-2 py-1 text-center font-medium">Key</th>{/if}</tr>
          </thead>
          <tbody>
            {#each columns as c, i (i)}
              <tr class="border-t border-line {picked[i] ? '' : 'text-faint'}">
                <td class="px-2 py-1"><input type="checkbox" aria-label="Export {c}" bind:checked={picked[i]} /></td>
                <td class="px-2 py-1 font-mono">{c}</td>
                <td class="px-2 py-1 font-mono text-[11.5px] text-faint">{types[i] ?? ''}</td>
                {#if mode === 'upsert'}<td class="px-2 py-1 text-center"><input type="checkbox" aria-label="{c} is a key column" bind:checked={key[i]} /></td>{/if}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </div>

    <div class="grid grid-cols-[3fr_2fr] gap-2">
      <label class="block space-y-1.5"><span class="label">Row already in target</span>
        <select class="input" bind:value={mode}>
          <option value="insert">Fail (plain INSERT)</option>
          <option value="skip">Skip it ({skipText})</option>
          <option value="upsert">Update it ({upsertText})</option>
        </select>
      </label>
      <label class="block space-y-1.5"><span class="label">Rows per statement</span><input class="input font-mono tabular-nums" type="number" min="1" max="10000" bind:value={rows} /></label>
    </div>
    {#if mode === 'upsert'}
      <p class="text-[12px] text-faint">{engine === 'mysql' ? 'MySQL finds the row by the primary key or a unique key of the target. Key columns are not updated.' : 'Key columns must match a primary key or unique constraint of the target. Other exported columns are updated.'}</p>
    {/if}

    <div class="flex flex-wrap gap-x-5 gap-y-2 text-[12.5px]">
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={transaction} /> Wrap in a transaction</label>
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={header} /> Header comment</label>
    </div>
  </div>

  {#snippet footer()}
    {#if problem}<span class="mr-auto min-w-0 truncate text-[12px] text-red-700 dark:text-red-400" title={problem}>{problem}</span>{/if}
    <button class="btn btn-secondary" onclick={() => answer(null)} disabled={busy}>Cancel</button>
    <button class="btn btn-primary" disabled={!!problem || busy} onclick={submit}>Export…</button>
  {/snippet}
</Modal>
