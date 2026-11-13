<script lang="ts">
  import { untrack } from 'svelte';
  import { Columns2, KeyRound, Link, Link2, ListTree, FileCode, Network, ShieldCheck, SlidersHorizontal, Gauge, LoaderCircle, RotateCw, CircleAlert, Plus, Pencil, Trash2 } from '@lucide/svelte';
  import { catalog, loadColumns, loadDdl, loadProperties, structureChanged } from '../catalog.svelte';
  import { canWrite, type Conn } from '../connections.svelte';
  import { displayName, canAlterConstraints, columnDef, constraintDef, foreignKeyDef, indexDef, indexEdit, type SchemaChange, type TableRef } from '../tableActions';
  import { tableKey } from '../tableId';
  import SchemaChangeDialog, { type SchemaAsk, type SchemaEdit } from './SchemaChangeDialog.svelte';
  import MongoSchemaDialog, { type MongoAsk } from './MongoSchemaDialog.svelte';
  import ErDiagram from './ErDiagram.svelte';
  import { ask as askDialog } from '../ask.svelte';

  /** `onopen`: open a related table from the ER diagram. */
  let { conn, table, onopen }: { conn: Conn; table: TableRef; onopen?: (t: TableRef) => void } = $props();

  type Section = 'columns' | 'constraints' | 'foreignKeys' | 'references' | 'indexes' | 'ddl' | 'diagram' | 'validator' | 'options' | 'stats';
  const sqlSections: { id: Section; label: string; icon: typeof Columns2 }[] = [
    { id: 'columns', label: 'Columns', icon: Columns2 },
    { id: 'constraints', label: 'Constraints', icon: KeyRound },
    { id: 'foreignKeys', label: 'Foreign Keys', icon: Link },
    { id: 'references', label: 'References', icon: Link2 },
    { id: 'indexes', label: 'Indexes', icon: ListTree },
    { id: 'ddl', label: 'DDL', icon: FileCode },
    { id: 'diagram', label: 'ER Diagram', icon: Network },
  ];
  let section = $state<Section>('columns');

  const mongo = $derived(conn.engine === 'mongo');
  const isView = $derived(table.kind !== 'table');
  const sections = $derived<{ id: Section; label: string; icon: typeof Columns2 }[]>(
    !mongo
      ? sqlSections
      : [
          { id: 'columns', label: 'Fields', icon: Columns2 },
          ...(isView ? [] : [{ id: 'indexes' as const, label: 'Indexes', icon: ListTree }]),
          ...(isView ? [] : [{ id: 'validator' as const, label: 'Validator', icon: ShieldCheck }]),
          { id: 'options' as const, label: 'Options', icon: SlidersHorizontal },
          ...(isView ? [] : [{ id: 'stats' as const, label: 'Stats', icon: Gauge }]),
          { id: 'ddl' as const, label: 'Script', icon: FileCode },
        ],
  );

  // One catalog entry per table: the Data view's foreign keys and this view share it, and a structure
  // change reloads it in place (the catalog's revision covers the views that watch it).
  const key = untrack(() => tableKey({ connId: conn.id, database: table.database, schema: table.schema, name: table.name }));
  const columns = $derived(catalog.columns[key]?.items ?? null);
  const info = $derived(catalog.properties[key]?.items ?? null);
  const ddl = $derived(catalog.ddl[key]?.items ?? null);
  const loads = $derived([catalog.columns[key], catalog.properties[key], catalog.ddl[key]]);
  const error = $derived(loads.find((l) => l?.status === 'error')?.error ?? '');
  const loading = $derived(loads.some((l) => l?.status === 'loading') || loads.every((l) => !l));

  // One load fills every section, so switching sections never waits.
  function load(force = false) {
    void Promise.all([
      loadColumns(conn.id, table.database, table.schema, table.name, force),
      loadProperties(conn.id, table.database, table.schema, table.name, force),
      loadDdl(conn.id, table.database, table.schema, table.name, force),
    ]);
  }
  load();

  const count = (s: Section) =>
    s === 'columns' ? columns?.length : s === 'ddl' || s === 'diagram' || s === 'validator' || s === 'options' || s === 'stats' ? undefined : info?.[s].length;
  // The collection itself on MongoDB: validator, options and stats.
  const coll = $derived(info?.mongo ?? null);
  const fkTarget = (schema: string | undefined, t: string) => (schema ? `${schema}.${t}` : t);
  const rule = (r: string) => (r === 'NO ACTION' ? '—' : r);

  /** Bytes as a short readable size: 892 MB, 1.4 kB. */
  function bytes(n: number) {
    const units = ['B', 'kB', 'MB', 'GB', 'TB'];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
  }

  // Views and read-only connections show the structure only.
  const editable = $derived(table.kind === 'table' && canWrite(conn));
  const addable: Partial<Record<Section, 'column' | 'constraint' | 'foreignKey' | 'index'>> = { columns: 'column', constraints: 'constraint', foreignKeys: 'foreignKey', indexes: 'index' };
  const addLabel = { column: 'Add column', constraint: 'Add constraint', foreignKey: 'Add foreign key', index: 'Add index' };
  // SQLite keeps constraints and foreign keys inside CREATE TABLE; they cannot change afterwards.
  const locked = (s: Section) => (s === 'constraints' || s === 'foreignKeys') && !canAlterConstraints(conn.engine);
  // A collection adds indexes only: its fields come from the documents, not from a schema.
  const add = $derived(mongo ? (section === 'indexes' ? 'index' : undefined) : addable[section]);
  const dropCol = $derived(editable && !!add && !locked(section));

  async function runChange(a: SchemaAsk) {
    if (await askDialog<boolean>(SchemaChangeDialog, { conn, table, columns: columns ?? [], ask: a })) afterChange();
  }

  async function runMongoChange(a: MongoAsk) {
    if (await askDialog<boolean>(MongoSchemaDialog, { conn, table, columns: columns ?? [], ask: a })) afterChange();
  }

  function afterChange() {
    // The catalog reloads the loaded columns, properties and DDL of the connection and the tree's lists.
    structureChanged(conn);
  }

  const askDrop = (drop: SchemaChange, label: string) => void runChange({ drop, label });
  // A collection has its own dialog: the SQL one would write CREATE INDEX text the Mongo parser refuses.
  function askAdd(kind: 'column' | 'constraint' | 'foreignKey' | 'index') {
    if (mongo && kind === 'index') void runMongoChange({ add: 'index' });
    else void runChange({ add: kind });
  }
</script>

{#snippet empty(text: string)}
  <div class="px-3 py-6 text-center text-[12.5px] text-faint">{text}</div>
{/snippet}

{#snippet actions(edit: SchemaEdit | null, drop: SchemaChange | null, label: string)}
  <td class="w-16 !px-1 text-right">
    {#if edit}<button class="btn btn-ghost size-6 p-0 text-faint opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:text-ink" title="Edit {label}" aria-label="Edit {label}" onclick={() => runChange({ edit })}><Pencil size={13} /></button>{/if}
    {#if drop}<button class="btn btn-ghost size-6 p-0 text-faint opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:text-red-600 dark:hover:text-red-400" title="Drop {label}" aria-label="Drop {label}" onclick={() => askDrop(drop, label)}><Trash2 size={13} /></button>{/if}
  </td>
{/snippet}

<div class="flex min-h-0 min-w-0 flex-1">
  <nav class="flex w-44 shrink-0 flex-col gap-0.5 border-r border-line p-1.5" aria-label="Table properties">
    {#each sections as s (s.id)}
      {@const n = count(s.id)}
      <button class="nav-item {section === s.id ? 'nav-item-active' : ''}" aria-current={section === s.id ? 'page' : undefined} onclick={() => (section = s.id)}>
        <s.icon size={13} class="shrink-0 {section === s.id ? 'text-accent' : 'text-faint'}" />
        <span class="flex-1">{s.label}</span>
        {#if n != null}<span class="text-[11px] text-faint tabular-nums">{n}</span>{/if}
      </button>
    {/each}
    <div class="mt-auto flex items-center gap-1.5 px-1 pt-2 text-[11.5px] text-faint">
      <span class="min-w-0 flex-1 truncate" title={displayName(table)}>{displayName(table)}</span>
      <button class="btn btn-ghost size-6 p-0" title="Refresh properties" aria-label="Refresh properties" disabled={loading} onclick={() => load(true)}>
        {#if loading}<LoaderCircle size={13} class="animate-spin" />{:else}<RotateCw size={13} />{/if}
      </button>
    </div>
  </nav>

  <div class="flex min-h-0 min-w-0 flex-1 flex-col">
  {#if editable && !error && columns}
    {#if mongo && section === 'validator'}
      <div class="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
        <button class="btn btn-ghost h-7 px-2" onclick={() => runMongoChange({ validator: { validator: coll?.validator ?? '', level: coll?.validationLevel ?? '', action: coll?.validationAction ?? '' } })}>
          {#if coll?.validator}<Pencil size={13} /> Edit validator{:else}<Plus size={13} /> Add validator{/if}
        </button>
      </div>
    {:else if add}
      <div class="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
        {#if locked(section)}
          <span class="px-1 text-[12px] text-faint">SQLite cannot add or drop {section === 'constraints' ? 'constraints' : 'foreign keys'} on an existing table.</span>
        {:else}
          <button class="btn btn-ghost h-7 px-2" onclick={() => askAdd(add)}><Plus size={13} /> {addLabel[add]}</button>
        {/if}
      </div>
    {/if}
  {/if}
  <div class="min-h-0 min-w-0 flex-1 overflow-auto">
    {#if error}
      <div class="m-4 flex items-start gap-2.5 rounded-lg bg-red-500/8 px-3.5 py-3 text-[12.5px] text-red-700 dark:text-red-400" role="alert">
        <CircleAlert size={15} class="mt-px shrink-0" /> <span>{error}</span>
      </div>
    {:else if loading && !columns}
      <div class="grid h-full place-items-center text-faint"><LoaderCircle size={16} class="animate-spin" /></div>
    {:else if section === 'ddl'}
      <pre class="p-3 font-mono text-[12px] leading-relaxed whitespace-pre-wrap text-ink">{ddl ?? ''}</pre>
    {:else if mongo && section === 'options'}
      <pre class="p-3 font-mono text-[12px] leading-relaxed whitespace-pre-wrap text-ink">{coll?.options ?? '{}'}</pre>
    {:else if mongo && section === 'stats'}
      {#if coll?.stats}
        <dl class="grid max-w-sm grid-cols-2 gap-x-6 gap-y-2 p-4 text-[12.5px]">
          <dt class="text-muted">Documents</dt><dd class="text-right font-mono text-ink tabular-nums">{coll.stats.count.toLocaleString()}</dd>
          <dt class="text-muted">Data size</dt><dd class="text-right font-mono text-ink tabular-nums" title={`${coll.stats.size} bytes`}>{bytes(coll.stats.size)}</dd>
          <dt class="text-muted">Average document</dt><dd class="text-right font-mono text-ink tabular-nums">{bytes(coll.stats.avgObjSize)}</dd>
          <dt class="text-muted">Storage size</dt><dd class="text-right font-mono text-ink tabular-nums">{bytes(coll.stats.storageSize)}</dd>
          <dt class="text-muted">Index size</dt><dd class="text-right font-mono text-ink tabular-nums">{bytes(coll.stats.totalIndexSize)}</dd>
          <dt class="text-muted">Indexes</dt><dd class="text-right font-mono text-ink tabular-nums">{coll.stats.nindexes}</dd>
        </dl>
      {:else}
        {@render empty('No stats: the server does not allow reading them')}
      {/if}
    {:else if mongo && section === 'validator'}
      {#if coll?.validator}
        <div class="flex flex-wrap items-center gap-1.5 px-3 pt-3">
          {#if coll.validationLevel}<span class="tag">level {coll.validationLevel}</span>{/if}
          {#if coll.validationAction}<span class="tag">action {coll.validationAction}</span>{/if}
        </div>
        <pre class="p-3 font-mono text-[12px] leading-relaxed whitespace-pre-wrap text-ink">{coll.validator}</pre>
      {:else}
        {@render empty('No validator')}
      {/if}
    {:else if mongo && section === 'indexes'}
      <table class="w-full border-separate border-spacing-0 text-[12.5px]">
        <thead class="sticky top-0 z-10 bg-surface text-left">
          <tr class="[&>th]:h-8 [&>th]:border-b [&>th]:border-line [&>th]:px-3 [&>th]:font-medium [&>th]:whitespace-nowrap [&>th]:text-muted">
            <th>Name</th><th>Keys</th><th>Flags</th><th>Type</th><th>Definition</th>{#if dropCol}<th></th>{/if}
          </tr>
        </thead>
        <tbody class="[&_td]:h-8 [&_td]:border-b [&_td]:border-line/60 [&_td]:px-3 [&_td]:whitespace-nowrap [&_tr:hover]:bg-hover">
          {#each info?.indexes ?? [] as x (x.name)}
            {@const m = x.mongo}
            {@const flags = [x.unique ? 'unique' : '', m?.sparse ? 'sparse' : '', m?.hidden ? 'hidden' : '', m?.ttlSeconds !== undefined ? `TTL ${m.ttlSeconds}s` : '', m?.partialFilter ? 'partial' : ''].filter(Boolean)}
            <tr class="group">
              <td class="font-medium text-ink">{x.name}{#if x.primary} <span class="tag ml-1 text-accent">PK</span>{/if}</td>
              <td class="font-mono text-[12px]">{m?.keys ?? x.columns.join(', ')}</td>
              <td class="text-muted">{flags.join(', ') || '—'}</td>
              <td class="text-muted">{x.method ?? '—'}</td>
              <td class="max-w-[28rem] truncate font-mono text-[12px] text-muted" title={x.definition}>{x.definition ?? ''}</td>
              {#if dropCol}
                <td class="w-16 !px-1 text-right">
                  <!-- MongoDB cannot change an index's keys: a change drops it and adds it again. -->
                  {#if !x.primary}
                    <button class="btn btn-ghost size-6 p-0 text-faint opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:text-red-600 dark:hover:text-red-400" title="Drop index {x.name}" aria-label="Drop index {x.name}" onclick={() => runMongoChange({ drop: { name: x.name, keys: m?.keys ?? x.columns.join(', ') } })}><Trash2 size={13} /></button>
                  {/if}
                </td>
              {/if}
            </tr>
          {:else}<tr><td colspan={5 + +dropCol}>{@render empty('No indexes')}</td></tr>{/each}
        </tbody>
      </table>
    {:else if mongo}
      {@const n = columns?.[0]?.sampleSize}
      <div class="flex h-8 items-center border-b border-line px-3 text-[12px] text-faint">{n === undefined ? 'No documents' : `Fields from a sample of ${n} document${n === 1 ? '' : 's'}`}</div>
      <table class="w-full border-separate border-spacing-0 text-[12.5px]">
        <thead class="sticky top-0 z-10 bg-surface text-left">
          <tr class="[&>th]:h-8 [&>th]:border-b [&>th]:border-line [&>th]:px-3 [&>th]:font-medium [&>th]:whitespace-nowrap [&>th]:text-muted">
            <th class="w-10">#</th><th>Field</th><th>Type</th><th>In documents</th><th>Key</th>
          </tr>
        </thead>
        <tbody class="[&_td]:h-8 [&_td]:border-b [&_td]:border-line/60 [&_td]:px-3 [&_td]:whitespace-nowrap [&_tr:hover]:bg-hover">
          {#each columns ?? [] as c, i (c.name)}
            <tr>
              <td class="text-faint tabular-nums">{i + 1}</td>
              <td class="font-medium text-ink">{c.name}</td>
              <td class="font-mono text-[12px] text-muted">{c.dataType}</td>
              <td class="text-muted tabular-nums">{c.frequency === undefined ? '' : `${Math.round(c.frequency * 100)}%`}</td>
              <td>{#if c.isPk}<span class="tag text-accent"><KeyRound size={11} /> _id</span>{/if}</td>
            </tr>
          {:else}<tr><td colspan="5">{@render empty('No fields')}</td></tr>{/each}
        </tbody>
      </table>
    {:else if section === 'diagram'}
      {#if columns && info}<ErDiagram {conn} {table} {columns} {info} {onopen} />{/if}
    {:else}
      <table class="w-full border-separate border-spacing-0 text-[12.5px]">
        <thead class="sticky top-0 z-10 bg-surface text-left">
          <tr class="[&>th]:h-8 [&>th]:border-b [&>th]:border-line [&>th]:px-3 [&>th]:font-medium [&>th]:whitespace-nowrap [&>th]:text-muted">
            {#if section === 'columns'}
              <th class="w-10">#</th><th>Name</th><th>Type</th><th>Not null</th><th>Default</th><th>Key</th>{#if dropCol}<th></th>{/if}
            {:else if section === 'constraints'}
              <th>Name</th><th>Type</th><th>Columns</th><th>Definition</th>{#if dropCol}<th></th>{/if}
            {:else if section === 'foreignKeys' || section === 'references'}
              <th>Name</th>{#if section === 'references'}<th>Table</th>{/if}<th>Columns</th><th>{section === 'references' ? 'Referenced columns' : 'Referenced table'}</th>{#if section === 'foreignKeys'}<th>Referenced columns</th>{/if}<th>On update</th><th>On delete</th>{#if dropCol}<th></th>{/if}
            {:else}
              <th>Name</th><th>Columns</th><th>Unique</th><th>Method</th><th>Definition</th>{#if dropCol}<th></th>{/if}
            {/if}
          </tr>
        </thead>
        <tbody class="[&_td]:h-8 [&_td]:border-b [&_td]:border-line/60 [&_td]:px-3 [&_td]:whitespace-nowrap [&_tr:hover]:bg-hover">
          {#if section === 'columns'}
            {#each columns ?? [] as c, i (c.name)}
              <tr class="group">
                <td class="text-faint tabular-nums">{i + 1}</td>
                <td class="font-medium text-ink">{c.name}</td>
                <td class="font-mono text-[12px] text-muted">{c.dataType}</td>
                <td>{c.nullable ? '' : '✓'}</td>
                <td class="font-mono text-[12px] text-muted">{c.default ?? ''}</td>
                <td>{#if c.isPk}<span class="tag text-accent"><KeyRound size={11} /> PK{c.pkOrdinal && c.pkOrdinal > 1 ? ` ${c.pkOrdinal}` : ''}</span>{/if}</td>
                {#if dropCol}{@render actions({ kind: 'column', from: columnDef(conn.engine, c) }, { op: 'dropColumn', name: c.name }, `column "${c.name}"`)}{/if}
              </tr>
            {:else}<tr><td colspan={6 + +dropCol}>{@render empty('No columns')}</td></tr>{/each}
          {:else if section === 'constraints'}
            {#each info?.constraints ?? [] as c (c.name)}
              <tr class="group">
                <td class="font-medium text-ink">{c.name}</td>
                <td><span class="tag">{c.kind === 'primary' ? 'PRIMARY KEY' : c.kind.toUpperCase()}</span></td>
                <td class="font-mono text-[12px]">{c.columns.join(', ')}</td>
                <td class="font-mono text-[12px] text-muted">{c.definition ?? ''}</td>
                {#if dropCol}
                  {@const from = constraintDef(c)}
                  {@render actions(from && { kind: 'constraint', from }, { op: 'dropConstraint', kind: c.kind, name: c.name }, `constraint "${c.name}"`)}
                {/if}
              </tr>
            {:else}<tr><td colspan={4 + +dropCol}>{@render empty('No constraints')}</td></tr>{/each}
          {:else if section === 'foreignKeys'}
            {#each info?.foreignKeys ?? [] as f (f.name)}
              <tr class="group">
                <td class="font-medium text-ink">{f.name}</td>
                <td class="font-mono text-[12px]">{f.columns.join(', ')}</td>
                <td>{fkTarget(f.refSchema, f.refTable)}</td>
                <td class="font-mono text-[12px]">{f.refColumns.join(', ')}</td>
                <td class="text-muted">{rule(f.onUpdate)}</td>
                <td class="text-muted">{rule(f.onDelete)}</td>
                {#if dropCol}{@render actions({ kind: 'foreignKey', from: foreignKeyDef(table, f) }, { op: 'dropForeignKey', name: f.name }, `foreign key "${f.name}"`)}{/if}
              </tr>
            {:else}<tr><td colspan={6 + +dropCol}>{@render empty('No foreign keys')}</td></tr>{/each}
          {:else if section === 'references'}
            {#each info?.references ?? [] as f (`${f.schema}.${f.table}.${f.name}`)}
              <tr>
                <td class="font-medium text-ink">{f.name}</td>
                <td>{fkTarget(f.schema, f.table)}</td>
                <td class="font-mono text-[12px]">{f.columns.join(', ')}</td>
                <td class="font-mono text-[12px]">{f.refColumns.join(', ')}</td>
                <td class="text-muted">{rule(f.onUpdate)}</td>
                <td class="text-muted">{rule(f.onDelete)}</td>
              </tr>
            {:else}<tr><td colspan="6">{@render empty('No other table references this table')}</td></tr>{/each}
          {:else}
            {#each info?.indexes ?? [] as x (x.name)}
              <tr class="group">
                <td class="font-medium text-ink">{x.name}{#if x.primary} <span class="tag ml-1 text-accent">PK</span>{/if}</td>
                <td class="font-mono text-[12px]">{x.columns.join(', ')}</td>
                <td>{x.unique ? '✓' : ''}</td>
                <td class="text-muted">{x.method ?? ''}</td>
                <td class="max-w-[28rem] truncate font-mono text-[12px] text-muted" title={x.definition}>{x.definition ?? ''}</td>
                {#if dropCol}
                  {@const how = indexEdit(conn.engine, x, columns ?? [])}
                  <!-- The primary key's index goes with its constraint. -->
                  {@render actions(how && { kind: 'index', from: indexDef(conn.engine, x), renameOnly: how === 'rename' }, x.primary ? null : { op: 'dropIndex', name: x.name }, `index "${x.name}"`)}
                {/if}
              </tr>
            {:else}<tr><td colspan={5 + +dropCol}>{@render empty('No indexes')}</td></tr>{/each}
          {/if}
        </tbody>
      </table>
    {/if}
  </div>
  </div>
</div>
