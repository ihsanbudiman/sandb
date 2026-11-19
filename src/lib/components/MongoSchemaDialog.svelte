<script lang="ts" module>
  import type { MongoValidatorDraft } from '../tableActions';

  /** `add`: a new index. `drop`: an index to drop. `validator`: the validator form, filled from the current one. */
  export type MongoAsk = { add: 'index' } | { drop: { name: string; keys: string } } | { validator: MongoValidatorDraft };
</script>

<script lang="ts">
  import { untrack } from 'svelte';
  import { Plus, Pencil, Trash } from '@lucide/svelte';
  import { api, type ColumnInfo } from '../api';
  import type { Conn } from '../connections.svelte';
  import { validatorPrefs } from '../dialogPrefs.svelte';
  import { mongoSchemaText, MONGO_INDEX_KINDS, MONGO_VALIDATION_LEVELS, MONGO_VALIDATION_ACTIONS, type MongoChange, type MongoIndexField, type TableRef } from '../tableActions';
  import SchemaFormDialog, { type SchemaDraft } from './SchemaFormDialog.svelte';
  import Select from './ui/Select.svelte';

  const indexKinds = MONGO_INDEX_KINDS.map((value) => ({ value, label: value }));
  const levels = [{ value: '', label: 'Default' }, ...MONGO_VALIDATION_LEVELS.map((value) => ({ value, label: value }))];
  const actions = [{ value: '', label: 'Default' }, ...MONGO_VALIDATION_ACTIONS.map((value) => ({ value, label: value }))];

  let {
    conn,
    table,
    columns,
    ask,
    answer,
  }: { conn: Conn; table: TableRef; columns: ColumnInfo[]; ask: MongoAsk; answer: (changed: boolean) => void } = $props();

  const e = untrack(() => conn.engine);
  const edit = untrack(() => ('validator' in ask ? ask.validator : null));
  const drop = untrack(() => ('drop' in ask ? ask.drop : null));
  const hasValidator = !!edit?.validator.trim();

  let fields = $state<MongoIndexField[]>([{ field: '', kind: '1' }]);
  let name = $state('');
  let unique = $state(false);
  let sparse = $state(false);
  let hidden = $state(false);
  let ttl = $state('');
  let partial = $state('');
  let validatorText = $state(edit?.validator ?? '');
  // An existing validator keeps its own level and action, '' meaning Default; a new one starts from the last.
  let level = $state(hasValidator && edit ? edit.level : validatorPrefs.level);
  let action = $state(hasValidator && edit ? edit.action : validatorPrefs.action);
  let remove = $state(false);

  const filled = $derived(fields.filter((f) => f.field.trim()));
  const validatorOk = $derived(/^\{[\s\S]*\}$/.test(validatorText.trim()));
  // Why a typed value cannot build the command yet; the reason shows instead of a bare disabled button.
  const indexProblem = $derived.by((): string | null => {
    // MongoDB keeps expireAfterSeconds on single-field indexes only.
    if (ttl.trim() && filled.length > 1) return 'TTL needs exactly one field';
    if (ttl.trim() && !/^\d+$/.test(ttl.trim())) return 'TTL seconds must be a whole number';
    if (partial.trim() && !/^\{[\s\S]*\}$/.test(partial.trim())) return 'The partial filter must be one { ... } document';
    return null;
  });
  const validatorProblem = $derived(!remove && validatorText.trim() && !validatorOk ? 'The validator must be one { ... } document' : null);
  const indexOk = $derived(filled.length > 0 && !indexProblem);
  // What the two free-text inputs look like; both hold a mongosh object.
  const validatorHint = '{\n  "$jsonSchema": { "bsonType": "object" }\n}';
  const partialHint = "{ status: 'paid' }";
  // A sampled array field is `items[].sku`; an index path writes `items.sku`.
  const indexPath = (field: string) => field.replace(/\[\]\./g, '.').replace(/\[\]/g, '');

  // The current form as one change; null while a required part is empty or malformed.
  const change = $derived.by((): MongoChange | null => {
    if (drop) return { op: 'dropIndex', name: drop.name };
    if (edit) {
      const v = { validator: validatorText, level, action };
      if (remove) return { op: 'removeValidator', validator: v };
      return validatorOk ? { op: 'setValidator', validator: v } : null;
    }
    return indexOk ? { op: 'createIndex', index: { name, fields: filled, unique, sparse, hidden, ttl, partial } } : null;
  });
  const command = $derived(change ? mongoSchemaText(table, change) : '');

  function build(): SchemaDraft {
    const problem = indexProblem ?? validatorProblem;
    if (command) return { sql: command };
    return { sql: null, problem: problem ?? undefined, preview: problem ? 'Fix the value above to see the command.' : 'Fill in the form to see the command.' };
  }

  // The free text is spliced into the command, so it could end the document and start another statement.
  // The classifier runs the real parser: one statement and no parse warning, or nothing runs.
  async function precheck(text: string): Promise<string | null> {
    const info = await api.classifyWrite({ engine: 'mongo', sql: text, database: null });
    const count = info.statements.length;
    if (count > 1) return `The text splits into ${count} statements, and only one may run. Nothing ran.`;
    // The parser is all or nothing: text it cannot read yields no statements, and its warning says where.
    const unparsable = info.warnings.find((w) => w.code === 'unparsable');
    if (unparsable || count !== 1) return unparsable ? `The text is not one command: ${unparsable.message}. Nothing ran.` : 'The text is not one command. Nothing ran.';
    return null;
  }

  function done() {
    if (edit) Object.assign(validatorPrefs, { level, action });
    answer(true);
  }
</script>

  {#snippet form()}
    {#if drop}
      <p class="leading-relaxed text-muted">This removes the index with keys <span class="font-mono text-sm text-ink">{drop.keys}</span> from the collection. The documents stay.</p>
    {:else if edit}
      {#if remove}
        <p class="leading-relaxed text-muted">This removes the validator. The documents stay; new writes are no longer checked.</p>
      {:else}
        <label class="block space-y-1.5">
          <span class="label">Validator <span class="font-normal text-faint">· the validator document as mongosh text</span></span>
          <textarea class="input h-56 py-1.5 font-mono text-sm" data-autofocus bind:value={validatorText} placeholder={validatorHint} autocapitalize="off" spellcheck="false"></textarea>
        </label>
      {/if}
      <div class="grid grid-cols-2 gap-2">
        <div class="space-y-1.5">
          <label for="mongo-validator-level" class="label block">Validation level</label>
          <Select id="mongo-validator-level" aria-label="Validation level" size="sm" class="font-mono" bind:value={level} options={levels} />
        </div>
        <div class="space-y-1.5">
          <label for="mongo-validator-action" class="label block">Validation action</label>
          <Select id="mongo-validator-action" aria-label="Validation action" size="sm" class="font-mono" bind:value={action} options={actions} />
        </div>
      </div>
      {#if hasValidator}<label class="flex items-center gap-2 text-sm"><input type="checkbox" bind:checked={remove} /> Remove the validator</label>{/if}
    {:else}
      <div class="space-y-1.5">
        <span class="label">Keys <span class="font-normal text-faint">· order follows the rows</span></span>
        {#each fields as f, i (i)}
          <div class="grid grid-cols-[1fr_9rem_auto] items-center gap-2">
            <input class="input input-sm font-mono" list="mongo-index-fields" placeholder="field" aria-label="Field {i + 1}" data-autofocus={i === 0 ? '' : undefined} bind:value={f.field} autocapitalize="off" spellcheck="false" autocomplete="off" />
            <Select aria-label="Kind of field {i + 1}" size="sm" class="font-mono" bind:value={f.kind} options={indexKinds} />
            <button type="button" class="btn btn-sm btn-ghost w-6 px-0 text-faint enabled:hover:text-danger" aria-label="Remove field {i + 1}" disabled={fields.length === 1} onclick={() => fields.splice(i, 1)}><Trash size={12} /></button>
          </div>
        {/each}
        <button type="button" class="btn btn-sm btn-ghost" onclick={() => fields.push({ field: '', kind: '1' })}><Plus size={12} /> Add field</button>
        <datalist id="mongo-index-fields">{#each columns as c (c.name)}<option value={indexPath(c.name)} label={c.name}></option>{/each}</datalist>
      </div>
      <div class="grid grid-cols-[2fr_1fr] gap-2">
        <label class="block space-y-1.5"><span class="label">Name <span class="font-normal text-faint">· empty lets MongoDB pick</span></span><input class="input input-sm font-mono" bind:value={name} autocapitalize="off" spellcheck="false" autocomplete="off" /></label>
        <label class="block space-y-1.5"><span class="label">TTL seconds <span class="font-normal text-faint">· empty for none</span></span><input class="input input-sm font-mono tabular-nums" bind:value={ttl} placeholder="none" inputmode="numeric" autocomplete="off" /></label>
      </div>
      <div class="flex flex-wrap gap-x-5 gap-y-2 text-sm">
        <label class="flex items-center gap-2"><input type="checkbox" bind:checked={unique} /> Unique</label>
        <label class="flex items-center gap-2"><input type="checkbox" bind:checked={sparse} /> Sparse</label>
        <label class="flex items-center gap-2"><input type="checkbox" bind:checked={hidden} /> Hidden</label>
      </div>
      <label class="block space-y-1.5">
        <span class="label">Partial filter <span class="font-normal text-faint">· mongosh object, inserted as written</span></span>
        <textarea class="input h-16 py-1.5 font-mono text-sm" bind:value={partial} placeholder={partialHint} autocapitalize="off" spellcheck="false"></textarea>
      </label>
    {/if}
  {/snippet}

<SchemaFormDialog
  {conn}
  {table}
  fields={form}
  title={drop ? `Drop index "${drop.name}"?` : edit ? (hasValidator ? 'Edit validator' : 'Add validator') : 'Add index'}
  button={drop ? 'Drop' : edit ? 'Save' : 'Add index'}
  destructive={!!drop}
  width="max-w-xl"
  {build}
  {precheck}
  onclose={() => answer(false)}
  ondone={done}
>
  {#snippet icon()}
    {#if drop}<Trash size={15} class="text-danger" />{:else if edit}<Pencil size={15} class="text-accent" />{:else}<Plus size={15} class="text-accent" />{/if}
  {/snippet}


  {#snippet notes()}
    <p class="text-sm leading-relaxed text-faint">The command runs immediately and commits.{#if !edit && !drop}{' '}MongoDB cannot change an index's keys, so a change means dropping and adding it.{/if}</p>
  {/snippet}
</SchemaFormDialog>
