<script lang="ts">
  import { ListFilter, Plus, X } from '@lucide/svelte';
  import { FILTER_OPERATORS, filterOperator, isTextType, type TableFilter } from '../tableActions';
  import { emptyMongoFilter, validateMongoFilter } from '../mongo/filter';
  import type { Engine } from '../api';
  import Select from './ui/Select.svelte';
  import MongoFilterInput from './MongoFilterInput.svelte';

  let {
    engine,
    columns,
    types,
    fields = columns,
    filters = $bindable([]),
    mongoFilter = $bindable(''),
    loading = false,
    onapply,
  }: {
    engine: Engine;
    columns: string[];
    types: string[];
    /** MongoDB schema paths for completion; the flattened result columns are the fallback. */
    fields?: string[];
    filters?: TableFilter[];
    /** The Compass-style MongoDB filter document; SQL tables use `filters` instead. */
    mongoFilter?: string;
    loading?: boolean;
    onapply: () => void;
  } = $props();

  let editing = $state(false);
  const allOperators = FILTER_OPERATORS.map(({ value, label }) => ({ value, label }));
  const nonTextOperators = FILTER_OPERATORS.filter((op) => !op.text).map(({ value, label }) => ({ value, label }));
  // Mongo columns type their fields `string` (or a union containing it); SQL uses char/text/...
  const isText = (column: string) => isTextType(types[columns.indexOf(column)] ?? '');
  const mongoValidation = $derived(validateMongoFilter(mongoFilter));
  const mongoValid = $derived(mongoValidation.valid);

  function addFilter() {
    if (!columns.length) return;
    editing = true;
    filters = [...filters, { id: crypto.randomUUID(), column: columns[0], operator: '=', value: '' }];
  }

  function setColumn(filter: TableFilter, column: string) {
    filter.column = column;
    if (filterOperator(filter.operator).text && !isText(column)) filter.operator = '=';
  }

  function clearFilters() {
    filters = [];
    editing = false;
    onapply();
  }

  function applyMongo() {
    if (mongoValid && !loading) onapply();
  }

  function resetMongo() {
    mongoFilter = '';
    if (!loading) onapply();
  }
</script>

{#if engine === 'mongo'}
  <form class="shrink-0 border-b border-line bg-sunken/45" onsubmit={(e) => { e.preventDefault(); applyMongo(); }}>
    <div class="flex min-h-sub-toolbar items-center gap-2 px-2 py-1">
      <span class="inline-flex shrink-0 items-center gap-1.5 text-xs font-medium text-muted"><ListFilter size={13} /> Filter</span>
      <MongoFilterInput bind:value={mongoFilter} fields={fields} invalid={!mongoValid} error={mongoValidation.valid ? '' : mongoValidation.error} onapply={applyMongo} />
      <button class="btn btn-sm btn-ghost shrink-0" type="button" disabled={loading || emptyMongoFilter(mongoFilter)} onclick={resetMongo}>Reset</button>
      <button class="btn btn-sm btn-secondary shrink-0" type="submit" disabled={!mongoValid || loading}>Find</button>
    </div>
  </form>
{:else}
<form class="shrink-0 border-b border-line bg-sunken/45" onsubmit={(e) => { e.preventDefault(); onapply(); }}>
  <div class="flex h-sub-toolbar items-center gap-2 px-2">
    {#if !editing && !filters.length}
      <button class="btn btn-sm btn-ghost" type="button" disabled={!columns.length || loading} onclick={addFilter}>
        <ListFilter size={13} /> Filter
      </button>
    {:else}
      <span class="inline-flex items-center gap-1.5 text-xs font-medium text-muted"><ListFilter size={13} /> Filters <span class="tabular-nums text-faint">{filters.length}</span></span>
      <button class="btn btn-sm btn-ghost" type="button" disabled={!columns.length || loading} onclick={addFilter}>
        <Plus size={13} /> Add filter
      </button>
      <div class="flex-1"></div>
      <button class="btn btn-sm btn-ghost" type="button" disabled={loading} onclick={clearFilters}>Clear</button>
      <button class="btn btn-sm btn-secondary" type="submit" disabled={!columns.length || loading}>Apply</button>
    {/if}
  </div>
  {#if filters.length}
    <div class="max-h-36 space-y-1.5 overflow-auto px-2 pb-2.5">
      {#each filters as filter, i (filter.id)}
        {@const op = filterOperator(filter.operator)}
        <div class="grid grid-cols-[minmax(5.5rem,0.9fr)_minmax(5.5rem,0.9fr)_minmax(4rem,1.2fr)_1.75rem] items-center gap-1.5">
          <div class="flex min-w-0 items-center gap-1.5">
            <span class="w-8 shrink-0 text-right text-2xs font-medium text-faint">{i ? 'AND' : 'WHERE'}</span>
            <select class="input input-sm min-w-0 flex-1" aria-label="Filter column" value={filter.column} onchange={(e) => setColumn(filter, e.currentTarget.value)}>
              {#each columns as column}<option value={column}>{column}</option>{/each}
            </select>
          </div>
          <Select class="w-full min-w-0" size="sm" aria-label="Filter operator" bind:value={filter.operator} options={isText(filter.column) ? allOperators : nonTextOperators} />
          {#if op.noValue}
            <span class="px-2 text-xs italic text-faint">No value needed</span>
          {:else}
            <input class="input input-sm w-full min-w-0 font-mono" aria-label="Filter value" bind:value={filter.value} placeholder="Value" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
          {/if}
          <button class="btn btn-sm btn-ghost size-6 p-0" type="button" title="Remove filter" aria-label="Remove filter {i + 1}" onclick={() => { editing = true; filters = filters.filter((f) => f.id !== filter.id); }}><X size={13} /></button>
        </div>
      {/each}
    </div>
  {/if}
</form>
{/if}
