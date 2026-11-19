<script lang="ts">
  import { untrack } from 'svelte';
  import { FileCode } from '@lucide/svelte';
  import { errorText, type ExportResult, type SqlOptions } from '../api';
  import { EXPORT_CONFLICT, MAX_ROWS_PER_STATEMENT } from '../api/generated/constants';
  import { exportSqlPrefs } from '../dialogPrefs.svelte';
  import type { SqlEngine } from '../sql/text';
  import Modal from './Modal.svelte';
  import Select from './ui/Select.svelte';

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
    engine: SqlEngine;
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
  const conflict = $derived(EXPORT_CONFLICT[engine]);
  const modes = $derived([
    { value: 'insert', label: 'Fail (plain INSERT)' },
    { value: 'skip', label: `Skip it (${conflict.skip})` },
    { value: 'upsert', label: `Update it (${conflict.upsert})` },
  ] as const);

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
      <label class="block space-y-1.5"><span class="label">{schemaLabel} <span class="font-normal text-faint">· empty to omit</span></span><input class="input input-sm font-mono" data-autofocus bind:value={schema} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
      <label class="block space-y-1.5"><span class="label">Target table</span><input class="input input-sm font-mono" bind:value={table} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
    </div>

    <div class="space-y-1.5">
      <div class="flex items-center gap-2">
        <span class="label flex-1">Columns <span class="font-normal text-faint">· {chosen.length} of {columns.length}</span></span>
        <button class="btn btn-sm btn-ghost" onclick={() => setAll(true)}>All</button>
        <button class="btn btn-sm btn-ghost" onclick={() => setAll(false)}>None</button>
      </div>
      <div class="max-h-52 overflow-auto rounded-lg border border-line">
        <table class="w-full text-sm">
          <thead class="sticky top-0 z-sticky bg-sunken text-left">
            <tr><th class="w-8 px-2 py-1"></th><th scope="col" class="th px-2 py-1">Column</th><th scope="col" class="th px-2 py-1">Type</th>{#if mode === 'upsert'}<th scope="col" class="th w-12 px-2 py-1 text-center">Key</th>{/if}</tr>
          </thead>
          <tbody>
            {#each columns as c, i (i)}
              <tr class="border-t border-line {picked[i] ? '' : 'text-faint'}">
                <td class="px-2 py-1"><input type="checkbox" aria-label="Export {c}" bind:checked={picked[i]} /></td>
                <td class="px-2 py-1 font-mono">{c}</td>
                <td class="px-2 py-1 font-mono text-xs text-faint">{types[i] ?? ''}</td>
                {#if mode === 'upsert'}<td class="px-2 py-1 text-center"><input type="checkbox" aria-label="{c} is a key column" bind:checked={key[i]} /></td>{/if}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </div>

    <div class="grid grid-cols-[3fr_2fr] gap-2">
      <div class="space-y-1.5">
        <label for="sql-export-mode" class="label block">Row already in target</label>
        <Select id="sql-export-mode" aria-label="Row already in target" size="sm" bind:value={mode} options={modes} />
      </div>
      <label class="block space-y-1.5"><span class="label">Rows per statement</span><input class="input input-sm font-mono tabular-nums" type="number" min="1" max="10000" bind:value={rows} /></label>
    </div>
    {#if mode === 'upsert'}
      <p class="text-sm text-faint">{engine === 'mysql' ? 'MySQL finds the row by the primary key or a unique key of the target. Key columns are not updated.' : 'Key columns must match a primary key or unique constraint of the target. Other exported columns are updated.'}</p>
    {/if}

    <div class="flex flex-wrap gap-x-5 gap-y-2 text-sm">
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={transaction} /> Wrap in a transaction</label>
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={header} /> Header comment</label>
    </div>
  </div>

  {#snippet footer()}
    {#if problem}<span class="mr-auto min-w-0 truncate text-xs text-danger" title={problem}>{problem}</span>{/if}
    <button class="btn btn-sm btn-secondary" onclick={() => answer(null)} disabled={busy}>Cancel</button>
    <button class="btn btn-sm btn-primary" disabled={!!problem || busy} onclick={submit}>Export…</button>
  {/snippet}
</Modal>
