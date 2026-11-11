<script lang="ts" module>
  import type { InsertMode } from '../api';

  // The statement settings carry over to the next export in this session; target and columns belong to the result.
  const last = { mode: 'insert' as InsertMode, rowsPerStatement: 100, transaction: false, header: true };
</script>

<script lang="ts">
  import { X, FileCode } from '@lucide/svelte';
  import { ApiError, type Engine, type SqlExportOptions } from '../api';

  /** `key`: default conflict key (the primary key when known), as result column numbers. */
  let {
    columns,
    types = [],
    engine,
    schema: schema0,
    table: table0,
    key: key0,
    onexport,
    onclose,
  }: {
    columns: string[];
    types?: string[];
    engine: Engine;
    schema: string | null;
    table: string;
    key: number[];
    onexport: (schema: string | null, table: string, options: SqlExportOptions) => Promise<void>;
    onclose: () => void;
  } = $props();

  // Initial values only: the dialog is created for one export.
  let schema = $state((() => schema0 ?? '')());
  let table = $state((() => table0)());
  let picked = $state<boolean[]>((() => columns.map(() => true))());
  let key = $state<boolean[]>((() => columns.map((_, i) => key0.includes(i)))());
  let mode = $state(last.mode);
  let rows = $state(last.rowsPerStatement);
  let transaction = $state(last.transaction);
  let header = $state(last.header);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const chosen = $derived(picked.flatMap((p, i) => (p ? [i] : [])));
  const keyCols = $derived(key.flatMap((k, i) => (k ? [i] : [])));
  const schemaLabel = $derived(engine === 'mysql' ? 'Database' : 'Schema');
  const problem = $derived.by(() => {
    if (!table.trim()) return 'Name the target table';
    if (!chosen.length) return 'Choose at least one column';
    if (!Number.isInteger(rows) || rows < 1 || rows > 10000) return 'Rows per statement must be from 1 to 10000';
    if (mode === 'upsert') {
      if (engine !== 'mysql' && !keyCols.length) return 'Choose the key columns that find an existing row';
      if (keyCols.some((k) => !picked[k])) return 'Export every key column';
    }
    return null;
  });
  // MongoDB results never open this dialog; its entry only completes the map.
  const skipText = $derived({ pg: 'ON CONFLICT DO NOTHING', mysql: 'INSERT IGNORE', sqlite: 'INSERT OR IGNORE', mongo: '' }[engine]);
  const upsertText = $derived(engine === 'mysql' ? 'ON DUPLICATE KEY UPDATE' : 'ON CONFLICT (key) DO UPDATE');

  function setAll(on: boolean) {
    picked = columns.map(() => on);
  }

  async function submit() {
    if (problem || busy) return;
    Object.assign(last, { mode, rowsPerStatement: rows, transaction, header });
    busy = true;
    error = null;
    try {
      await onexport(schema.trim() || null, table.trim(), {
        columns: chosen.length === columns.length ? null : chosen,
        mode,
        key: mode === 'upsert' ? keyCols : [],
        rowsPerStatement: rows,
        transaction,
        header,
      });
    } catch (e) {
      error = ApiError.from(e).message;
    } finally {
      busy = false;
    }
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape' && !busy) onclose();
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="dialog-backdrop">
  <div class="dialog max-w-xl">
    <header class="dialog-head">
      <FileCode size={15} class="text-faint" />
      <h2 class="flex-1">Export as SQL</h2>
      <button class="btn btn-ghost -my-1 -mr-1.5 size-7 p-0" onclick={onclose} disabled={busy} aria-label="Close"><X size={15} /></button>
    </header>

    <div class="dialog-body space-y-4">
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

    <footer class="dialog-foot">
      {#if problem ?? error}<span class="mr-auto min-w-0 truncate text-[12px] text-red-700 dark:text-red-400" title={problem ?? error}>{problem ?? error}</span>{/if}
      <button class="btn btn-secondary" onclick={onclose} disabled={busy}>Cancel</button>
      <button class="btn btn-primary" disabled={!!problem || busy} onclick={submit}>Export…</button>
    </footer>
  </div>
</div>
