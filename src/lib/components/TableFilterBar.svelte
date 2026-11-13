<script lang="ts">
  import { ListFilter, Plus, X } from '@lucide/svelte';
  import { FILTER_OPERATORS, filterOperator, isTextType, type TableFilter } from '../tableActions';

  let {
    columns,
    types,
    filters = $bindable([]),
    loading = false,
    onapply,
  }: { columns: string[]; types: string[]; filters?: TableFilter[]; loading?: boolean; onapply: () => void } = $props();

  const control = 'h-7 rounded-md border border-line bg-field px-2 text-[12px] text-ink outline-none focus:border-accent/70 focus:ring-2 focus:ring-accent/15';
  // Mongo columns type their fields `string` (or a union containing it); SQL uses char/text/...
  const isText = (column: string) => isTextType(types[columns.indexOf(column)] ?? '');

  function addFilter() {
    if (!columns.length) return;
    filters = [...filters, { id: crypto.randomUUID(), column: columns[0], operator: '=', value: '' }];
  }

  function setColumn(filter: TableFilter, column: string) {
    filter.column = column;
    if (filterOperator(filter.operator).text && !isText(column)) filter.operator = '=';
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
        {@const op = filterOperator(filter.operator)}
        <div class="grid grid-cols-[minmax(110px,0.8fr)_minmax(120px,0.65fr)_minmax(140px,1.4fr)_28px] items-center gap-2">
          <div class="flex min-w-0 items-center gap-2">
            <span class="w-8 shrink-0 text-right text-[10.5px] font-medium uppercase tracking-wide text-faint">{i ? 'AND' : 'WHERE'}</span>
            <select class="{control} min-w-0 flex-1" aria-label="Filter column" value={filter.column} onchange={(e) => setColumn(filter, e.currentTarget.value)}>
              {#each columns as column}<option value={column}>{column}</option>{/each}
            </select>
          </div>
          <select class="{control} w-full" aria-label="Filter operator" bind:value={filter.operator}>
            {#each FILTER_OPERATORS as operator}
              {#if !operator.text || isText(filter.column)}<option value={operator.value}>{operator.label}</option>{/if}
            {/each}
          </select>
          {#if op.noValue}
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
