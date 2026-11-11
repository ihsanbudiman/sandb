<script lang="ts" module>
  import type { MongoValidatorDraft } from '../tableActions';

  /** `add`: a new index. `drop`: an index to drop. `validator`: the validator form, filled from the current one. */
  export type MongoAsk = { add: 'index' } | { drop: { name: string; keys: string } } | { validator: MongoValidatorDraft };

  // The last level and action carry over to the next validator dialog in this session.
  const last = { level: 'strict', action: 'error' };
</script>

<script lang="ts">
  import { untrack } from 'svelte';
  import { Plus, Pencil, Trash, ShieldAlert, LoaderCircle } from '@lucide/svelte';
  import { engines, envTag, type Conn } from '../state.svelte';
  import { api, ApiError, type ColumnInfo } from '../api';
  import { logQuery } from '../audit.svelte';
  import { mongoSchemaText, MONGO_INDEX_KINDS, MONGO_VALIDATION_LEVELS, MONGO_VALIDATION_ACTIONS, displayName, type MongoChange, type MongoIndexField, type TableRef } from '../tableActions';

  let { conn, table, columns, ask, onclose, ondone }: {
    conn: Conn;
    table: TableRef;
    columns: ColumnInfo[];
    ask: MongoAsk;
    onclose: () => void;
    ondone: () => void;
  } = $props();

  const e = untrack(() => conn.engine);
  const edit = untrack(() => ('validator' in ask ? ask.validator : null));
  const drop = untrack(() => ('drop' in ask ? ask.drop : null));
  const hasValidator = !!edit?.validator.trim();

  // Form state; the dialog mounts fresh for each ask.
  let fields = $state<MongoIndexField[]>([{ field: '', kind: '1' }]);
  let name = $state('');
  let unique = $state(false);
  let sparse = $state(false);
  let hidden = $state(false);
  let ttl = $state('');
  let partial = $state('');
  let validatorText = $state(edit?.validator ?? '');
  // An existing validator keeps its own level and action, '' meaning Default; a new one starts from the last.
  let level = $state(hasValidator && edit ? edit.level : last.level);
  let action = $state(hasValidator && edit ? edit.action : last.action);
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
  const title = $derived(drop ? `Drop index "${drop.name}"?` : edit ? (hasValidator ? 'Edit validator' : 'Add validator') : 'Add index');

  const prod = $derived(conn.env === 'production');
  let ack = $state(false);
  let busy = $state(false);
  let error = $state('');

  // Through the pool without a tabId: the change commits at once, also on Production (the checkbox asks first).
  async function run() {
    if (!command || busy) return;
    busy = true;
    error = '';
    const started = Date.now();
    try {
      // The free text is spliced into the command, so it could end the document and start another statement.
      // The classifier runs the real parser: one statement and no parse warning, or nothing runs.
      const info = await api.classifyWrite('mongo', command);
      const count = info.statements.length;
      if (count > 1) {
        error = `The text splits into ${count} statements, and only one may run. Nothing ran.`;
        return;
      }
      // The parser is all or nothing: text it cannot read yields no statements, and its warning says where.
      const unparsable = info.warnings.find((w) => w.code === 'unparsable');
      if (unparsable || count !== 1) {
        error = unparsable ? `The text is not one command: ${unparsable.message}. Nothing ran.` : 'The text is not one command. Nothing ran.';
        return;
      }
      await api.runQuery({ connId: conn.id, database: table.database, sql: command, queryId: crypto.randomUUID(), limit: 1 });
      logQuery({ kind: 'WRITE', source: 'You', sql: command, result: { status: 'ok', rows: 0 }, ms: Date.now() - started, conn });
      if (edit) Object.assign(last, { level, action });
      ondone();
    } catch (err) {
      const x = ApiError.from(err);
      logQuery({ kind: 'WRITE', source: 'You', sql: command, result: { status: 'error', message: x.message }, ms: Date.now() - started, conn });
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

<svelte:window onkeydown={(ev) => { if (!busy && ev.key === 'Escape') { ev.preventDefault(); onclose(); } }} />

<div class="dialog-backdrop">
  <button class="absolute inset-0 cursor-default" aria-label="Close" tabindex="-1" onclick={() => !busy && onclose()}></button>
  <div class="dialog max-w-xl" role={drop ? 'alertdialog' : 'dialog'} aria-labelledby="mongo-schema-title">
  <form onsubmit={submit}>
    <header class="dialog-head">
      {#if drop}<Trash size={15} class="text-red-600 dark:text-red-400" />{:else if edit}<Pencil size={15} class="text-accent" />{:else}<Plus size={15} class="text-accent" />{/if}
      <h2 id="mongo-schema-title" class="flex-1">{title}</h2>
    </header>

    <div class="dialog-body max-h-[70vh] space-y-3 overflow-y-auto">
      <p class="flex flex-wrap items-center gap-2 rounded-lg bg-sunken px-3 py-2">
        <span class="font-mono text-[10.5px] font-semibold {engines[e].badge}">{engines[e].short}</span>
        <b class="font-medium text-ink">{conn.name}</b>
        <span class="tag {envTag[conn.env]}">{conn.env}</span>
        <span class="font-mono text-[12px] text-muted">{table.database} · {displayName(table)}</span>
      </p>

      {#if drop}
        <p class="leading-relaxed text-muted">This removes the index with keys <span class="font-mono text-[12px] text-ink">{drop.keys}</span> from the collection. The documents stay.</p>
      {:else if edit}
        {#if remove}
          <p class="leading-relaxed text-muted">This removes the validator. The documents stay; new writes are no longer checked.</p>
        {:else}
          <label class="block space-y-1.5">
            <span class="label">Validator <span class="font-normal text-faint">· the validator document as mongosh text</span></span>
            <textarea class="input h-56 py-1.5 font-mono text-[12px]" bind:value={validatorText} use:focus placeholder={validatorHint} autocapitalize="off" spellcheck="false"></textarea>
          </label>
        {/if}
        <div class="grid grid-cols-2 gap-2">
          <label class="block space-y-1.5"><span class="label">Validation level</span>
            <select class="input font-mono" bind:value={level}>
              <option value="">Default</option>
              {#each MONGO_VALIDATION_LEVELS as l (l)}<option value={l}>{l}</option>{/each}
            </select>
          </label>
          <label class="block space-y-1.5"><span class="label">Validation action</span>
            <select class="input font-mono" bind:value={action}>
              <option value="">Default</option>
              {#each MONGO_VALIDATION_ACTIONS as a (a)}<option value={a}>{a}</option>{/each}
            </select>
          </label>
        </div>
        {#if hasValidator}<label class="flex items-center gap-2 text-[12.5px]"><input type="checkbox" bind:checked={remove} /> Remove the validator</label>{/if}
      {:else}
        <div class="space-y-1.5">
          <span class="label">Keys <span class="font-normal text-faint">· order follows the rows</span></span>
          {#each fields as f, i (i)}
            <div class="grid grid-cols-[1fr_9rem_auto] items-center gap-2">
              <input class="input h-7 font-mono text-[12px]" list="mongo-index-fields" placeholder="field" aria-label="Field {i + 1}" bind:value={f.field} autocapitalize="off" spellcheck="false" autocomplete="off" />
              <select class="input h-7 font-mono text-[12px]" aria-label="Kind of field {i + 1}" bind:value={f.kind}>
                {#each MONGO_INDEX_KINDS as k (k)}<option value={k}>{k}</option>{/each}
              </select>
              <button type="button" class="btn btn-ghost size-7 p-0 text-faint hover:text-red-600 dark:hover:text-red-400" aria-label="Remove field {i + 1}" disabled={fields.length === 1} onclick={() => fields.splice(i, 1)}><Trash size={13} /></button>
            </div>
          {/each}
          <button type="button" class="btn btn-ghost h-6 px-1.5 text-[12px]" onclick={() => fields.push({ field: '', kind: '1' })}><Plus size={12} /> Add field</button>
          <datalist id="mongo-index-fields">{#each columns as c (c.name)}<option value={indexPath(c.name)} label={c.name}></option>{/each}</datalist>
        </div>
        <div class="grid grid-cols-[2fr_1fr] gap-2">
          <label class="block space-y-1.5"><span class="label">Name <span class="font-normal text-faint">· empty lets MongoDB pick</span></span><input class="input font-mono" bind:value={name} autocapitalize="off" spellcheck="false" autocomplete="off" /></label>
          <label class="block space-y-1.5"><span class="label">TTL seconds <span class="font-normal text-faint">· empty for none</span></span><input class="input font-mono tabular-nums" bind:value={ttl} placeholder="none" inputmode="numeric" autocomplete="off" /></label>
        </div>
        <div class="flex flex-wrap gap-x-5 gap-y-2 text-[12.5px]">
          <label class="flex items-center gap-2"><input type="checkbox" bind:checked={unique} /> Unique</label>
          <label class="flex items-center gap-2"><input type="checkbox" bind:checked={sparse} /> Sparse</label>
          <label class="flex items-center gap-2"><input type="checkbox" bind:checked={hidden} /> Hidden</label>
        </div>
        <label class="block space-y-1.5">
          <span class="label">Partial filter <span class="font-normal text-faint">· mongosh object, inserted as written</span></span>
          <textarea class="input h-16 py-1.5 font-mono text-[12px]" bind:value={partial} placeholder={partialHint} autocapitalize="off" spellcheck="false"></textarea>
        </label>
      {/if}

      {#if indexProblem || validatorProblem}
        <p class="rounded-lg bg-amber-500/10 px-3 py-2 text-[12px] leading-relaxed text-amber-800 dark:text-amber-300">{indexProblem ?? validatorProblem}</p>
      {/if}

      <p class="text-[12px] leading-relaxed text-faint">The command runs immediately and commits.{#if !edit && !drop}{' '}MongoDB cannot change an index's keys, so a change means dropping and adding it.{/if}</p>
      <pre class="overflow-x-auto rounded-lg bg-sunken px-3 py-2 font-mono text-[12px] whitespace-pre-wrap {command ? 'text-ink' : 'text-faint'}">{command || (indexProblem || validatorProblem ? 'Fix the value above to see the command.' : 'Fill in the form to see the command.')}</pre>

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
      <button type="submit" class="btn {drop ? 'bg-red-600 text-white enabled:hover:bg-red-500' : 'btn-primary'}" disabled={!command || (prod && !ack) || busy}>
        {#if busy}<LoaderCircle size={13} class="animate-spin" />{/if} {drop ? 'Drop' : edit ? 'Save' : 'Add index'}
      </button>
    </footer>
  </form>
  </div>
</div>
