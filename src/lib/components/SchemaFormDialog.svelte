<script lang="ts" module>
  /** What the form builds: the statement to run, or why it has none and what the preview says instead. */
  export type SchemaDraft = { sql: string } | { sql: null; problem?: string; preview: string };
</script>

<script lang="ts">
  import { LoaderCircle } from '@lucide/svelte';
  import type { Snippet } from 'svelte';
  import { errorText } from '../api';
  import type { Conn } from '../connections.svelte';
  import { runOnce } from '../run.svelte';
  import { displayName, type TableRef } from '../tableActions';
  import Modal from './Modal.svelte';
  import TargetBadge from './TargetBadge.svelte';
  import ProdAck, { ackBlocked } from './ProdAck.svelte';

  /**
   * The one form frame of a structure change: the target, the fields, the statement preview, the
   * production tick and the run. `precheck` refuses the statement text before anything runs.
   */
  let {
    conn,
    table,
    title,
    button,
    destructive = false,
    width = 'max-w-lg',
    build,
    precheck,
    icon,
    fields,
    notes,
    onclose,
    ondone,
  }: {
    conn: Conn;
    table: TableRef;
    title: string;
    /** The primary button label. */
    button: string;
    /** A drop: the alert role, the red button. */
    destructive?: boolean;
    width?: string;
    build: () => SchemaDraft;
    /** Answers with a reason to refuse the statement, or null to run it. */
    precheck?: (sql: string) => Promise<string | null>;
    icon: Snippet;
    fields: Snippet;
    notes?: Snippet;
    onclose: () => void;
    ondone: () => void;
  } = $props();

  const draft = $derived(build());
  const statement = $derived(draft.sql ?? '');
  const problem = $derived(draft.sql === null ? (draft.problem ?? null) : null);
  const preview = $derived(draft.sql === null ? draft.preview : '');
  const prod = $derived(conn.env === 'production');

  let ack = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  // Through the pool without a tabId: the change commits at once, also on Production (the tick asks first).
  async function run() {
    if (!statement || busy) return;
    const sql = statement;
    busy = true;
    error = null;
    try {
      const refused = precheck ? await precheck(sql) : null;
      if (refused) {
        error = refused;
        return;
      }
      await runOnce(conn, table.database, sql, 'WRITE');
      ondone();
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!ackBlocked(prod, ack, busy)) run();
  }
</script>

<Modal
  {title}
  role={destructive ? 'alertdialog' : 'dialog'}
  {width}
  {busy}
  {error}
  errorLabel="Failed:"
  bodyClass="max-h-[70vh] space-y-3 overflow-y-auto"
  onsubmit={submit}
  {onclose}
>
  {#snippet head()}{@render icon()}{/snippet}

  <TargetBadge {conn} target={table} />
  {@render fields()}
  {#if problem}
    <p class="rounded-lg bg-amber-500/10 px-3 py-2 text-[12px] leading-relaxed text-amber-800 dark:text-amber-300">{problem}</p>
  {/if}
  {@render notes?.()}
  <pre class="overflow-x-auto rounded-lg bg-sunken px-3 py-2 font-mono text-[12px] whitespace-pre-wrap {statement ? 'text-ink' : 'text-faint'}">{statement || preview}</pre>

  {#if prod}
    <ProdAck bind:checked={ack}>I want to change <b class="font-mono">{displayName(table)}</b>.</ProdAck>
  {/if}

  {#snippet footer()}
    <button type="button" class="btn btn-secondary" disabled={busy} onclick={onclose}>Cancel</button>
    <button type="submit" class="btn {destructive ? 'bg-red-600 text-white enabled:hover:bg-red-500' : 'btn-primary'}" disabled={!statement || ackBlocked(prod, ack, busy)}>
      {#if busy}<LoaderCircle size={13} class="animate-spin" />{/if} {button}
    </button>
  {/snippet}
</Modal>
