<script lang="ts" module>
  import type { SchemaChange, ColumnDef, ConstraintDef, ForeignKeyDef, IndexDef } from '../tableActions';
  export type SchemaKind = 'column' | 'constraint' | 'foreignKey' | 'index';
  /** The current object an edit starts from. */
  export type SchemaEdit = { kind: 'column'; from: ColumnDef } | { kind: 'constraint'; from: ConstraintDef } | { kind: 'foreignKey'; from: ForeignKeyDef } | { kind: 'index'; from: IndexDef; renameOnly: boolean };
  /** `add`: a form for a new object. `edit`: the same form filled from an existing one. `drop`: a ready change with a label such as `index "orders_idx"`. */
  export type SchemaAsk = { add: SchemaKind } | { edit: SchemaEdit } | { drop: SchemaChange; label: string };
</script>

<script lang="ts">
  import { untrack } from 'svelte';
  import { Plus, Pencil, Trash, ShieldAlert, LoaderCircle } from '@lucide/svelte';
  import { engines, envTag, type Conn } from '../state.svelte';
  import { api, ApiError, type ColumnInfo } from '../api';
  import { logQuery } from '../audit.svelte';
  import { schemaChangeSql, displayName, FK_RULES, type TableRef } from '../tableActions';

  let { conn, table, columns, ask, onclose, ondone }: {
    conn: Conn;
    table: TableRef;
    columns: ColumnInfo[];
    ask: SchemaAsk;
    onclose: () => void;
    ondone: () => void;
  } = $props();

  const e = untrack(() => conn.engine);
  const edit = untrack(() => ('edit' in ask ? ask.edit : null));
  const kind = untrack(() => ('add' in ask ? ask.add : edit?.kind ?? null));
  const titles = { column: 'Add column', constraint: 'Add constraint', foreignKey: 'Add foreign key', index: 'Add index' };
  const editTitles = { column: 'Edit column', constraint: 'Edit constraint', foreignKey: 'Edit foreign key', index: 'Edit index' };
  const title = (k: SchemaKind) => (edit ? editTitles[k] : titles[k]);

  // Form state, one set for every kind; the dialog mounts fresh for each ask. An edit fills it from the current object.
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

  const types: Record<typeof e, string[]> = {
    pg: ['integer', 'bigint', 'smallint', 'numeric(12,2)', 'text', 'varchar(255)', 'boolean', 'date', 'timestamp', 'timestamptz', 'uuid', 'jsonb', 'bytea'],
    mysql: ['int', 'bigint', 'smallint', 'tinyint(1)', 'decimal(12,2)', 'varchar(255)', 'text', 'date', 'datetime', 'timestamp', 'json', 'blob'],
    sqlite: ['INTEGER', 'REAL', 'TEXT', 'BLOB', 'NUMERIC'],
    // MongoDB tables never open this dialog; its entries only complete the maps.
    mongo: [],
  };
  const methods: Record<typeof e, string[]> = { pg: ['btree', 'hash', 'gin', 'gist', 'brin', 'spgist'], mysql: ['BTREE', 'HASH'], sqlite: [], mongo: [] };

  // Foreign key target: same database; PG may pick another schema.
  let schemas = $state<string[]>([]);
  let refSchema = $state(untrack(() => fk?.ref.schema ?? table.schema));
  let refTables = $state<string[]>([]);
  let refTable = $state(fk?.ref.name ?? '');
  let refCols = $state<ColumnInfo[]>([]);
  let refPicked = $state<string[]>([]);
  // An edit's first load keeps the foreign key's own referenced columns.
  let keepRef = !!fk;
  let refError = $state('');

  async function loadRefTables() {
    refError = '';
    try {
      const list = await api.listTables(conn.id, table.database, refSchema);
      refTables = list.filter((t) => t.kind === 'table').map((t) => t.name);
      if (!refTables.includes(refTable)) refTable = refTables[0] ?? '';
      await loadRefCols();
    } catch (err) {
      refError = ApiError.from(err).message;
    }
  }

  // The referenced primary key is the usual target, so it fills the column pairs.
  async function loadRefCols() {
    refCols = [];
    refPicked = [];
    if (!refTable) return;
    try {
      refCols = await api.listColumns(conn.id, table.database, refSchema, refTable);
      refPicked = keepRef && fk ? [...fk.refColumns] : refCols.filter((c) => c.isPk).sort((a, b) => (a.pkOrdinal ?? 0) - (b.pkOrdinal ?? 0)).map((c) => c.name);
    } catch (err) {
      refError = ApiError.from(err).message;
    } finally {
      keepRef = false;
    }
  }

  if (kind === 'foreignKey') {
    if (e === 'pg') untrack(() => api.listSchemas(conn.id, table.database)).then((s) => (schemas = s)).catch(() => {});
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
  const sql = $derived(change ? schemaChangeSql(e, table, change) : '');

  const prod = $derived(conn.env === 'production');
  const drop = 'drop' in untrack(() => ask);
  let ack = $state(false);
  let busy = $state(false);
  let error = $state('');

  // Through the pool without a tabId: the change commits at once, also on Production (the checkbox asks first).
  async function run() {
    if (!sql || busy) return;
    busy = true;
    error = '';
    const started = Date.now();
    try {
      await api.runQuery({ connId: conn.id, database: table.database, sql, queryId: crypto.randomUUID(), limit: 1 });
      logQuery({ kind: 'WRITE', source: 'You', sql, result: { status: 'ok', rows: 0 }, ms: Date.now() - started, conn });
      ondone();
    } catch (err) {
      const x = ApiError.from(err);
      logQuery({ kind: 'WRITE', source: 'You', sql, result: { status: 'error', message: x.message }, ms: Date.now() - started, conn });
      error = x.detail ? `${x.message}: ${x.detail}` : x.message;
    } finally {
      busy = false;
    }
  }

  function submit(ev: SubmitEvent) {
    ev.preventDefault();
    if (!(prod && !ack)) run();
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

<svelte:window onkeydown={(ev) => { if (!busy && ev.key === 'Escape') { ev.preventDefault(); onclose(); } }} />

<div class="dialog-backdrop">
  <button class="absolute inset-0 cursor-default" aria-label="Close" tabindex="-1" onclick={() => !busy && onclose()}></button>
  <div class="dialog max-w-lg" role={drop ? 'alertdialog' : 'dialog'} aria-labelledby="schema-change-title">
  <form onsubmit={submit}>
    <header class="dialog-head">
      {#if drop}<Trash size={15} class="text-red-600 dark:text-red-400" />{:else if edit}<Pencil size={15} class="text-accent" />{:else}<Plus size={15} class="text-accent" />{/if}
      <h2 id="schema-change-title" class="flex-1">{'drop' in ask ? `Drop ${ask.label}?` : title(kind ?? 'column')}</h2>
    </header>

    <div class="dialog-body max-h-[70vh] space-y-3 overflow-y-auto">
      <p class="flex flex-wrap items-center gap-2 rounded-lg bg-sunken px-3 py-2">
        <span class="font-mono text-[10.5px] font-semibold {engines[e].badge}">{engines[e].short}</span>
        <b class="font-medium text-ink">{conn.name}</b>
        <span class="tag {envTag[conn.env]}">{conn.env}</span>
        <span class="font-mono text-[12px] text-muted">{e === 'sqlite' ? '' : table.database + ' · '}{displayName(table)}</span>
      </p>

      {#if kind === 'column'}
        <div class="grid grid-cols-2 gap-2">
          <label class="block space-y-1.5"><span class="label">Name</span><input class="input font-mono" bind:value={name} use:focus autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
          <label class="block space-y-1.5"><span class="label">Type</span><input class="input font-mono" bind:value={type} list="schema-types" placeholder={types[e][0]} disabled={nameOnly} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
        </div>
        <datalist id="schema-types">{#each types[e] as t (t)}<option value={t}></option>{/each}</datalist>
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
        {#if refError}<p class="text-[12px] text-red-700 dark:text-red-400">{refError}</p>{/if}
      {:else if kind === 'index'}
        <label class="block space-y-1.5"><span class="label">Name</span><input class="input font-mono" bind:value={name} placeholder={suggested} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
        {#if nameOnly}
          <p class="text-[12px] leading-relaxed text-faint">This index has parts the form cannot show, such as expressions, sort order, WHERE, INCLUDE or FULLTEXT. You can change only the name.</p>
        {:else}
          {@render colPicker('Columns')}
          <div class="flex items-center gap-4">
            <label class="flex items-center gap-2 text-[12.5px]"><input type="checkbox" bind:checked={unique} /> Unique</label>
            {#if methods[e].length}
              <label class="flex items-center gap-2 text-[12.5px]">Method
                <select class="input h-7 w-32 font-mono text-[12px]" bind:value={method}>
                  <option value="">Default</option>
                  {#each methods[e] as m (m)}<option value={m}>{m}</option>{/each}
                </select>
              </label>
            {/if}
          </div>
        {/if}
      {:else}
        <p class="leading-relaxed text-muted">This removes the {'drop' in ask ? ask.label : ''} from the table.{#if 'drop' in ask && ask.drop.op === 'dropColumn'}{' '}The column's data is lost.{/if}</p>
      {/if}

      <p class="text-[12px] leading-relaxed text-faint">The statement runs immediately and commits.{#if sql.includes('BEGIN;')}{' '}All statements commit together, or none does.{/if}</p>
      {#if e === 'mysql' && sql.includes('CHANGE COLUMN')}
        <p class="rounded-lg bg-amber-500/10 px-3 py-2 text-[12px] leading-relaxed text-amber-800 dark:text-amber-300">CHANGE COLUMN writes the full column again. AUTO_INCREMENT, COMMENT and ON UPDATE are not in this form. If the column has them, they go away.</p>
      {/if}
      <pre class="overflow-x-auto rounded-lg bg-sunken px-3 py-2 font-mono text-[12px] whitespace-pre-wrap {sql ? 'text-ink' : 'text-faint'}">{sql || (edit && draft ? 'No changes.' : 'Fill in the form to see the SQL.')}</pre>

      {#if prod}
        <label class="flex cursor-pointer items-start gap-2.5 rounded-lg bg-red-500/8 px-3 py-2.5 text-[12.5px]">
          <input type="checkbox" class="mt-0.5 accent-red-600" bind:checked={ack} />
          <span><ShieldAlert size={13} class="mr-0.5 inline text-red-600 dark:text-red-400" /> This is a <b>production</b> connection. I want to change <b class="font-mono">{displayName(table)}</b>.</span>
        </label>
      {/if}

      {#if error}
        <p class="break-words rounded-lg bg-red-500/8 px-3 py-2 text-[12.5px] text-red-700 dark:text-red-300" role="alert"><b class="font-semibold">Failed:</b> {error}</p>
      {/if}
    </div>

    <footer class="dialog-foot">
      <button type="button" class="btn btn-secondary" disabled={busy} onclick={onclose}>Cancel</button>
      <button type="submit" class="btn {drop ? 'bg-red-600 text-white enabled:hover:bg-red-500' : 'btn-primary'}" disabled={!sql || (prod && !ack) || busy}>
        {#if busy}<LoaderCircle size={13} class="animate-spin" />{/if} {drop ? 'Drop' : edit ? 'Save' : titles[kind ?? 'column']}
      </button>
    </footer>
  </form>
  </div>
</div>
