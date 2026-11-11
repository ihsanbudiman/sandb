<script lang="ts">
  import { X, ShieldAlert, Save, FileCode, LoaderCircle, TriangleAlert } from '@lucide/svelte';
  import { engines, envTag, type Conn } from '../state.svelte';
  import type { Stmt } from '../edits.svelte';

  /**
   * `transactions`: false on a standalone MongoDB server, where the commands run one by one.
   * `saving`: the save is running; the dialog then cannot close, and `oncancel` stops it.
   * `error`: the save failed with this message; the dialog stays open so it reads here.
   */
  let {
    mode = $bindable(),
    stmts,
    conn,
    transactions = true,
    saving = false,
    error = null,
    onsave,
    oncancel = () => {},
  }: {
    mode: 'preview' | 'save' | null;
    stmts: Stmt[];
    conn: Conn;
    transactions?: boolean;
    saving?: boolean;
    error?: string | null;
    onsave: () => void;
    oncancel?: () => void;
  } = $props();

  let ack = $state(false);
  const prod = $derived(conn.env === 'production');
  const mongo = $derived(conn.engine === 'mongo');
  const kindTint = { UPDATE: 'text-amber-700 dark:text-amber-400', INSERT: 'text-emerald-700 dark:text-emerald-400', DELETE: 'text-red-700 dark:text-red-400' };
  // A Mongo command starts with its collection; the method name gets the tint.
  const mongoMethod = { UPDATE: '.updateOne(', INSERT: '.insertOne(', DELETE: '.deleteOne(' };

  $effect(() => {
    if (!mode) ack = false;
  });

  // Pending edits can be discarded underneath the dialog (connection switch, Discard); a save
  // in flight keeps the dialog open until it answers.
  $effect(() => {
    if (mode && !saving && stmts.length === 0) mode = null;
  });

  function onkey(e: KeyboardEvent) {
    if (mode && !saving && e.key === 'Escape') mode = null;
  }
</script>

<svelte:window onkeydown={onkey} />

{#if mode}
  <div class="dialog-backdrop">
    <div class="dialog max-w-xl">
      <header class="dialog-head">
        {#if mode === 'save'}<Save size={15} class="text-faint" />{:else}<FileCode size={15} class="text-faint" />{/if}
        <h2 class="flex-1">{mode === 'save' ? 'Save changes' : mongo ? 'Generated commands' : 'Generated SQL'}</h2>
        <button class="btn btn-ghost -my-1 -mr-1.5 size-7 p-0" disabled={saving} onclick={() => (mode = null)} aria-label="Close"><X size={15} /></button>
      </header>

      <div class="dialog-body">
        <div class="flex flex-wrap items-center gap-2">
          <span class="font-mono text-[10.5px] font-semibold {engines[conn.engine].badge}">{engines[conn.engine].short}</span>
          <span class="font-medium">{conn.name}</span>
          <span class="tag {envTag[conn.env]}">{conn.env}</span>
          <span class="ml-auto text-[12px] text-faint">{stmts.length} {mongo ? 'command' : 'statement'}{stmts.length === 1 ? '' : 's'}{transactions ? ' · one transaction' : ''}</span>
        </div>
        {#if !transactions}
          <p class="mt-2 flex items-start gap-2 rounded-lg bg-amber-400/12 px-3 py-2 text-[12.5px] text-amber-800 dark:text-amber-300">
            <TriangleAlert size={13} class="mt-0.5 shrink-0" />
            <span>This server is standalone and has no transactions. Changes run one by one; if one fails, the earlier ones stay saved.</span>
          </p>
        {/if}

        <div class="mt-2.5 max-h-72 overflow-auto rounded-lg border border-line bg-sunken px-3 py-2.5 font-mono text-[12px] leading-relaxed">
          {#each stmts as s, i}
            <div class="flex gap-2.5">
              <span aria-hidden="true" class="w-4 shrink-0 text-right text-faint select-none">{i + 1}</span>
              {#if mongo}
                {@const at = s.sql.indexOf(mongoMethod[s.kind]) + 1}
                <span class="min-w-0 whitespace-pre-wrap">{s.sql.slice(0, at)}<span class={kindTint[s.kind]}>{mongoMethod[s.kind].slice(1, -1)}</span>{s.sql.slice(at + mongoMethod[s.kind].length - 2)}</span>
              {:else}
                <span class="min-w-0 whitespace-pre-wrap"><span class={kindTint[s.kind]}>{s.kind}</span>{s.sql.slice(s.kind.length)}</span>
              {/if}
            </div>
          {/each}
        </div>

        {#if mode === 'save' && prod}
          <label class="mt-3 flex cursor-pointer items-start gap-2.5 rounded-lg bg-red-500/8 px-3 py-2.5 text-[12.5px]">
            <input type="checkbox" class="mt-0.5 accent-red-600" disabled={saving} bind:checked={ack} />
            <span><ShieldAlert size={13} class="mr-0.5 inline text-red-600 dark:text-red-400" /> This is a <b>production</b> connection. I want to write these changes to <b class="font-mono">{conn.name}</b>.</span>
          </label>
        {/if}
      </div>

      {#if error}
        <p class="mx-5 mb-1 rounded-lg bg-red-500/10 px-3 py-2 text-[12.5px] text-red-700 dark:text-red-300">{error}</p>
      {/if}

      <footer class="dialog-foot">
        {#if mode === 'save'}
          {#if saving}
            <span class="mr-auto flex items-center gap-2 text-[12.5px] text-muted"><LoaderCircle size={13} class="animate-spin text-accent" /> Saving changes…</span>
            <button class="btn btn-secondary" onclick={oncancel}>Cancel</button>
          {:else}
            <button class="btn btn-secondary" onclick={() => (mode = null)}>Cancel</button>
            <button class="btn btn-primary" disabled={prod && !ack} onclick={onsave}>Save {stmts.length} change{stmts.length === 1 ? '' : 's'}</button>
          {/if}
        {:else}
          <button class="btn btn-secondary" onclick={() => (mode = null)}>Close</button>
          <button class="btn btn-primary" onclick={() => (mode = 'save')}>Save…</button>
        {/if}
      </footer>
    </div>
  </div>
{/if}
