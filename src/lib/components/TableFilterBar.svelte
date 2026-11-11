<script lang="ts">
  import { ListFilter, Plus, X } from '@lucide/svelte';
  import type { TableFilter, TableFilterOperator } from '../tableActions';

  let {
    columns,
    types = [],
    filters = $bindable([]),
    loading = false,
    onapply,
  }: { columns: string[]; types?: string[]; filters?: TableFilter[]; loading?: boolean; onapply: () => void } = $props();

  const operators: { value: TableFilterOperator; label: string; text?: boolean }[] = [
    { value: '=', label: 'equals' },
    { value: '<>', label: 'does not equal' },
    { value: '>', label: 'greater than' },
    { value: '>=', label: 'at least' },
    { value: '<', label: 'less than' },
    { value: '<=', label: 'at most' },
    { value: 'contains', label: 'contains', text: true },
    { value: 'startsWith', label: 'starts with', text: true },
    { value: 'endsWith', label: 'ends with', text: true },
    { value: 'isNull', label: 'is NULL' },
    { value: 'isNotNull', label: 'is not NULL' },
  ];

  const control = 'h-7 rounded-md border border-line bg-field px-2 text-[12px] text-ink outline-none focus:border-accent/70 focus:ring-2 focus:ring-accent/15';
  // Mongo columns type their fields `string` (or a union containing it); SQL uses char/text/...
  const isText = (column: string) => /char|text|uuid|json|xml|citext|string/i.test(types[columns.indexOf(column)] ?? 'text');

  function addFilter() {
    if (!columns.length) return;
    filters = [...filters, { id: crypto.randomUUID(), column: columns[0], operator: '=', value: '' }];
  }

  function setColumn(filter: TableFilter, column: string) {
    filter.column = column;
    if (operators.find((o) => o.value === filter.operator)?.text && !isText(column)) filter.operator = '=';
  }

  function clearFilters() {
    filters = [];
    onapply();
  }
</script>

<form class="shrink-0 border-b border-line bg-sunken/45" onsubmit={(e) => { e.preventDefault(); onapply(); }}>
  <div class="flex h-10 items-center gap-2 px-3">
    <ListFilter size={14} class="shrink-0 text-muted" />
    <span class="text-[12px] font-medium text-muted">Filter</span>
    <button class="btn btn-ghost h-7 px-2 text-[12px]" type="button" disabled={!columns.length || loading} onclick={addFilter}>
      <Plus size={13} /> Add filter
    </button>
    {#if !filters.length}<span class="text-[12px] text-faint">Show all rows</span>{/if}
    <div class="flex-1"></div>
    {#if filters.length}<button class="btn btn-ghost h-7 px-2 text-[12px]" type="button" disabled={loading} onclick={clearFilters}>Clear</button>{/if}
    <button class="btn btn-secondary h-7 px-3 text-[12px]" type="submit" disabled={!columns.length || loading}>Apply</button>
  </div>
  {#if filters.length}
    <div class="space-y-1.5 px-3 pb-2.5">
      {#each filters as filter, i (filter.id)}
        <div class="grid grid-cols-[minmax(110px,0.8fr)_minmax(120px,0.65fr)_minmax(140px,1.4fr)_28px] items-center gap-2">
          <div class="flex min-w-0 items-center gap-2">
            <span class="w-8 shrink-0 text-right text-[10.5px] font-medium uppercase tracking-wide text-faint">{i ? 'AND' : 'WHERE'}</span>
            <select class="{control} min-w-0 flex-1" aria-label="Filter column" value={filter.column} onchange={(e) => setColumn(filter, e.currentTarget.value)}>
              {#each columns as column}<option value={column}>{column}</option>{/each}
            </select>
          </div>
          <select class="{control} w-full" aria-label="Filter operator" bind:value={filter.operator}>
            {#each operators as operator}
              {#if !operator.text || isText(filter.column)}<option value={operator.value}>{operator.label}</option>{/if}
            {/each}
          </select>
          {#if filter.operator === 'isNull' || filter.operator === 'isNotNull'}
            <span class="px-2 text-[12px] italic text-faint">No value needed</span>
          {:else}
            <input class="{control} w-full font-mono" aria-label="Filter value" bind:value={filter.value} placeholder="Value" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
          {/if}
          <button class="btn btn-ghost size-7 p-0" type="button" title="Remove filter" aria-label="Remove filter {i + 1}" onclick={() => (filters = filters.filter((f) => f.id !== filter.id))}><X size={13} /></button>
        </div>
      {/each}
    </div>
  {/if}
</form>
