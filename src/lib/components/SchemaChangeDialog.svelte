<script lang="ts" module>
  import type { SchemaChange, ColumnDef, ConstraintDef, ForeignKeyDef, IndexDef } from '../tableActions';
  export type SchemaKind = 'column' | 'constraint' | 'foreignKey' | 'index';
  export type SchemaEdit = { kind: 'column'; from: ColumnDef } | { kind: 'constraint'; from: ConstraintDef } | { kind: 'foreignKey'; from: ForeignKeyDef } | { kind: 'index'; from: IndexDef; renameOnly: boolean };
  /** `add`: a form for a new object. `edit`: the same form filled from an existing one. `drop`: a ready change with a label such as `index "orders_idx"`. */
  export type SchemaAsk = { add: SchemaKind } | { edit: SchemaEdit } | { drop: SchemaChange; label: string };
</script>

<script lang="ts">
  import { untrack } from 'svelte';
  import { Plus, Pencil, Trash } from '@lucide/svelte';
  import { api, errorText, type ColumnInfo } from '../api';
  import type { Conn } from '../connections.svelte';
  import { schemaChangeSql, FK_RULES, COLUMN_TYPES, INDEX_METHODS, type TableRef } from '../tableActions';
  import type { SqlEngine } from '../sql/text';
  import SchemaFormDialog, { type SchemaDraft } from './SchemaFormDialog.svelte';

  let {
    conn,
    table,
    columns,
    ask,
    answer,
  }: { conn: Conn; table: TableRef; columns: ColumnInfo[]; ask: SchemaAsk; answer: (changed: boolean) => void } = $props();

  const e = untrack(() => conn.engine) as SqlEngine;
  const edit = untrack(() => ('edit' in ask ? ask.edit : null));
  const kind = untrack(() => ('add' in ask ? ask.add : (edit?.kind ?? null)));
  const drop = 'drop' in untrack(() => ask);
  const titles = { column: 'Add column', constraint: 'Add constraint', foreignKey: 'Add foreign key', index: 'Add index' };
  const editTitles = { column: 'Edit column', constraint: 'Edit constraint', foreignKey: 'Edit foreign key', index: 'Edit index' };
  const title = (k: SchemaKind) => (edit ? editTitles[k] : titles[k]);

  const col = edit?.kind === 'column' ? edit.from : null;
  const con = edit?.kind === 'constraint' ? edit.from : null;
  const fk = edit?.kind === 'foreignKey' ? edit.from : null;
  const idx = edit?.kind === 'index' ? edit.from : null;
  let name = $state(edit?.from.name ?? '');
  let type = $state(col?.type ?? '');
  let notNull = $state(col?.notNull ?? false);
  let def = $state(col?.default ?? '');
  let cKind = $state<'unique' | 'primary' | 'check'>(con?.kind ?? 'unique');
  let check = $state(con?.check ?? '');
  let picked = $state<string[]>([...(con?.columns ?? fk?.columns ?? idx?.columns ?? [])]);
  let unique = $state(idx?.unique ?? false);
  let method = $state(idx?.method ?? '');
  let onUpdate = $state(fk?.onUpdate ?? 'NO ACTION');
  let onDelete = $state(fk?.onDelete ?? 'NO ACTION');
  // What the form may change: SQLite only renames a column; some indexes only take a new name; a MySQL primary key is always named PRIMARY.
  const nameOnly = (kind === 'column' && !!col && e === 'sqlite') || (edit?.kind === 'index' && edit.renameOnly);
  const fixedName = con?.kind === 'primary' && e === 'mysql';

  // Foreign key target: same database; PG may pick another schema.
  let schemas = $state<string[]>([]);
  let schemaError = $state('');
  let refSchema = $state(untrack(() => fk?.ref.schema ?? table.schema));
  let refTables = $state<string[]>([]);
  let refTable = $state(fk?.ref.name ?? '');
  let refCols = $state<ColumnInfo[]>([]);
  let refPicked = $state<string[]>([]);
  // An edit's first load keeps the foreign key's own referenced columns.
  let keepRef = !!fk;
  let refError = $state('');

  async function loadSchemas() {
    try {
      schemas = await api.listSchemas({ connId: conn.id, database: table.database });
    } catch (err) {
      schemaError = errorText(err);
    }
  }

  async function loadRefTables() {
    refError = '';
    try {
      const list = await api.listTables({ connId: conn.id, database: table.database, schema: refSchema });
      refTables = list.filter((t) => t.kind === 'table').map((t) => t.name);
      if (!refTables.includes(refTable)) refTable = refTables[0] ?? '';
      await loadRefCols();
    } catch (err) {
      refError = errorText(err);
    }
  }

  // The referenced primary key is the usual target, so it fills the column pairs.
  async function loadRefCols() {
    refCols = [];
    refPicked = [];
    if (!refTable) return;
    try {
      refCols = await api.listColumns({ connId: conn.id, database: table.database, schema: refSchema, table: refTable });
      refPicked = keepRef && fk ? [...fk.refColumns] : refCols.filter((c) => c.isPk).sort((a, b) => (a.pkOrdinal ?? 0) - (b.pkOrdinal ?? 0)).map((c) => c.name);
    } catch (err) {
      refError = errorText(err);
    } finally {
      keepRef = false;
    }
  }

  if (kind === 'foreignKey') {
    if (e === 'pg') loadSchemas();
    loadRefTables();
  }

  const toggle = (c: string) => (picked = picked.includes(c) ? picked.filter((x) => x !== c) : [...picked, c]);

  // PG's own naming pattern; the input shows it as the placeholder and an empty input uses it.
  const suggested = $derived.by(() => {
    const cols = picked.join('_');
    if (kind === 'index') return `${table.name}_${cols || 'col'}_idx`;
    if (kind === 'foreignKey') return `${table.name}_${cols || 'col'}_fkey`;
    if (kind === 'constraint') return cKind === 'primary' ? `${table.name}_pkey` : cKind === 'check' ? `${table.name}_check` : `${table.name}_${cols || 'col'}_key`;
    return '';
  });
  const objName = $derived(name.trim() || suggested);

  const draft = $derived.by((): SchemaChange | null => {
    if (kind === 'column') return name.trim() && type.trim() ? { op: 'addColumn', name, type, notNull, default: def } : null;
    if (kind === 'constraint') {
      if (cKind === 'check' ? !check.trim() : !picked.length) return null;
      return { op: 'addConstraint', kind: cKind, name: objName, columns: picked, check };
    }
    if (kind === 'foreignKey') {
      const refColumns = picked.map((_, i) => refPicked[i] ?? '');
      if (!picked.length || !refTable || refColumns.some((c) => !c)) return null;
      return { op: 'addForeignKey', name: objName, columns: picked, ref: { database: table.database, schema: refSchema, name: refTable, kind: 'table' }, refColumns, onUpdate, onDelete };
    }
    return picked.length ? { op: 'addIndex', name: objName, columns: picked, unique, method } : null;
  });
  const change = $derived.by((): SchemaChange | null => {
    if ('drop' in ask) return ask.drop;
    if (!edit || !draft) return draft;
    if (draft.op === 'addColumn' && col) return { op: 'editColumn', from: col, to: draft };
    if (draft.op === 'addConstraint' && con) return { op: 'editConstraint', from: con, to: draft };
    if (draft.op === 'addForeignKey' && fk) return { op: 'editForeignKey', from: fk, to: draft };
    if (draft.op === 'addIndex' && idx) return { op: 'editIndex', from: idx, to: draft };
    return null;
  });

  const built = $derived(change ? schemaChangeSql(e, table, change) : null);

  function build(): SchemaDraft {
    if (built?.sql) return { sql: built.sql };
    return { sql: null, preview: edit && draft ? 'No changes.' : 'Fill in the form to see the SQL.' };
  }

  const focus = (el: HTMLElement) => el.focus();
</script>

{#snippet colPicker(label: string)}
  <div class="space-y-1.5">
    <span class="label">{label} <span class="font-normal text-faint">· order follows your clicks</span></span>
    <div class="flex flex-wrap gap-1.5">
      {#each columns as c (c.name)}
        {@const i = picked.indexOf(c.name)}
        <button type="button" class="tag cursor-pointer gap-1 font-mono {i >= 0 ? 'bg-accent/15 text-accent' : 'hover:text-ink'}" aria-pressed={i >= 0} onclick={() => toggle(c.name)}>
          {#if i >= 0}<span class="tabular-nums">{i + 1}</span>{/if}{c.name}
        </button>
      {/each}
    </div>
  </div>
{/snippet}

<SchemaFormDialog
  {conn}
  {table}
  title={'drop' in ask ? `Drop ${ask.label}?` : title(kind ?? 'column')}
  button={drop ? 'Drop' : edit ? 'Save' : titles[kind ?? 'column']}
  destructive={drop}
  {build}
  onclose={() => answer(false)}
  ondone={() => answer(true)}
>
  {#snippet icon()}
    {#if drop}<Trash size={15} class="text-red-600 dark:text-red-400" />{:else if edit}<Pencil size={15} class="text-accent" />{:else}<Plus size={15} class="text-accent" />{/if}
  {/snippet}

  {#snippet fields()}
    {#if kind === 'column'}
      <div class="grid grid-cols-2 gap-2">
        <label class="block space-y-1.5"><span class="label">Name</span><input class="input font-mono" bind:value={name} use:focus autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
        <label class="block space-y-1.5"><span class="label">Type</span><input class="input font-mono" bind:value={type} list="schema-types" placeholder={COLUMN_TYPES[e][0]} disabled={nameOnly} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
      </div>
      <datalist id="schema-types">{#each COLUMN_TYPES[e] as t (t)}<option value={t}></option>{/each}</datalist>
      <label class="block space-y-1.5"><span class="label">Default <span class="font-normal text-faint">· SQL expression, e.g. 0, 'new' or now()</span></span><input class="input font-mono" bind:value={def} disabled={nameOnly} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
      <label class="flex items-center gap-2 text-[12.5px]"><input type="checkbox" bind:checked={notNull} disabled={nameOnly} /> Not null</label>
      {#if nameOnly}<p class="text-[12px] text-faint">SQLite can only rename a column.</p>{/if}
    {:else if kind === 'constraint'}
      <div class="seg w-fit" role="radiogroup" aria-label="Constraint type">
        {#each [['unique', 'Unique'], ['primary', 'Primary key'], ['check', 'Check']] as const as [v, l] (v)}
          <button type="button" class="seg-item {cKind === v ? 'seg-on' : ''}" role="radio" aria-checked={cKind === v} disabled={!!edit && cKind !== v} onclick={() => (cKind = v)}>{l}</button>
        {/each}
      </div>
      <label class="block space-y-1.5"><span class="label">Name</span><input class="input font-mono" bind:value={name} placeholder={suggested} disabled={fixedName} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
      {#if fixedName}<p class="text-[12px] text-faint">MySQL always names the primary key PRIMARY.</p>{/if}
      {#if cKind === 'check'}
        <label class="block space-y-1.5"><span class="label">Expression</span><textarea class="input h-20 py-1.5 font-mono" bind:value={check} placeholder="price >= 0" autocapitalize="off" spellcheck="false"></textarea></label>
      {:else}
        {@render colPicker('Columns')}
      {/if}
    {:else if kind === 'foreignKey'}
      <label class="block space-y-1.5"><span class="label">Name</span><input class="input font-mono" bind:value={name} placeholder={suggested} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
      {@render colPicker('Columns')}
      <div class="grid {e === 'pg' ? 'grid-cols-2' : 'grid-cols-1'} gap-2">
        {#if e === 'pg'}
          <label class="block space-y-1.5"><span class="label">Referenced schema</span>
            <select class="input font-mono" bind:value={refSchema} onchange={loadRefTables}>
              {#each schemas.length ? schemas : [refSchema] as s (s)}<option value={s}>{s}</option>{/each}
            </select>
          </label>
        {/if}
        <label class="block space-y-1.5"><span class="label">Referenced table</span>
          <select class="input font-mono" bind:value={refTable} onchange={loadRefCols}>
            {#each refTables as t (t)}<option value={t}>{t}</option>{/each}
          </select>
        </label>
      </div>
      {#if picked.length}
        <div class="space-y-1.5">
          <span class="label">Column pairs</span>
          {#each picked as c, i (c)}
            <div class="grid grid-cols-[1fr_auto_1fr] items-center gap-2">
              <span class="truncate font-mono text-[12px] text-ink">{c}</span>
              <span class="text-faint">→</span>
              <select class="input h-7 font-mono text-[12px]" aria-label="Referenced column for {c}" bind:value={refPicked[i]}>
                <option value="">Choose…</option>
                {#each refCols as r (r.name)}<option value={r.name}>{r.name}</option>{/each}
              </select>
            </div>
          {/each}
        </div>
      {/if}
      <div class="grid grid-cols-2 gap-2">
        <label class="block space-y-1.5"><span class="label">On update</span><select class="input" bind:value={onUpdate}>{#each FK_RULES as r (r)}<option value={r}>{r}</option>{/each}</select></label>
        <label class="block space-y-1.5"><span class="label">On delete</span><select class="input" bind:value={onDelete}>{#each FK_RULES as r (r)}<option value={r}>{r}</option>{/each}</select></label>
      </div>
      {#if schemaError}<p class="text-[12px] text-red-700 dark:text-red-400">{schemaError}</p>{/if}
      {#if refError}<p class="text-[12px] text-red-700 dark:text-red-400">{refError}</p>{/if}
    {:else if kind === 'index'}
      <label class="block space-y-1.5"><span class="label">Name</span><input class="input font-mono" bind:value={name} placeholder={suggested} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
      {#if nameOnly}
        <p class="text-[12px] leading-relaxed text-faint">This index has parts the form cannot show, such as expressions, sort order, WHERE, INCLUDE or FULLTEXT. You can change only the name.</p>
      {:else}
        {@render colPicker('Columns')}
        <div class="flex items-center gap-4">
          <label class="flex items-center gap-2 text-[12.5px]"><input type="checkbox" bind:checked={unique} /> Unique</label>
          {#if INDEX_METHODS[e].length}
            <label class="flex items-center gap-2 text-[12.5px]">Method
              <select class="input h-7 w-32 font-mono text-[12px]" bind:value={method}>
                <option value="">Default</option>
                {#each INDEX_METHODS[e] as m (m)}<option value={m}>{m}</option>{/each}
              </select>
            </label>
          {/if}
        </div>
      {/if}
    {:else}
      <p class="leading-relaxed text-muted">This removes the {'drop' in ask ? ask.label : ''} from the table.{#if 'drop' in ask && ask.drop.op === 'dropColumn'}{' '}The column's data is lost.{/if}</p>
    {/if}
  {/snippet}

  {#snippet notes()}
    <p class="text-[12px] leading-relaxed text-faint">The statement runs immediately and commits.{#if built?.atomic}{' '}All statements commit together, or none does.{/if}</p>
    {#if built?.rewritesColumn}
      <p class="rounded-lg bg-amber-500/10 px-3 py-2 text-[12px] leading-relaxed text-amber-800 dark:text-amber-300">CHANGE COLUMN writes the full column again. AUTO_INCREMENT, COMMENT and ON UPDATE are not in this form. If the column has them, they go away.</p>
    {/if}
  {/snippet}
</SchemaFormDialog>
