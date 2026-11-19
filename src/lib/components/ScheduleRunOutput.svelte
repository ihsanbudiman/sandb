<script lang="ts">
  import { FileCode, MessageSquare } from '@lucide/svelte';
  import Markdown from './Markdown.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import { chat, openSession } from '../chat.svelte';
  import { db } from '../connections.svelte';
  import { notify } from '../notice.svelte';
  import { formatCell } from '../results';
  import { RESULT_RUNS, STORED_ROWS, type Schedule, type ScheduleRun } from '../schedules.svelte';
  import { ui } from '../state.svelte';
  import { openSqlTab } from '../tabs.svelte';

  let { run, schedule }: { run: ScheduleRun; schedule: Schedule } = $props();

  const task = $derived(schedule.task.kind === 'query' ? schedule.task : null);
  const conn = $derived(task ? db.connections.find((c) => c.id === task.connId) : undefined);
  /** Runs at `RESULT_RUNS` and beyond keep only status and message; the store trimmed their rows/reply. */
  const trimmed = $derived(schedule.runs.indexOf(run) >= RESULT_RUNS);

  /** A column is numeric when its first non-null value is a number. */
  const nums = $derived.by(() => {
    const cols = run.columns ?? [];
    const rows = run.rows ?? [];
    return cols.map((_, i) => typeof rows.find((r) => r[i] != null)?.[i] === 'number');
  });

  const fmt = (v: unknown) => (typeof v === 'number' ? v.toLocaleString() : v !== null && typeof v === 'object' ? formatCell(v) : String(v));

  /** The chat a prompt run wrote stays until the user deletes it from the chat history. */
  const chatMissing = $derived(!run.chatId || !chat.sessions.some((s) => s.id === run.chatId));

  function openEditor() {
    if (!task || !conn) return;
    openSqlTab(conn, task.database || null, task.sql);
    ui.page = 'query';
  }

  function openChat() {
    if (chat.live) return notify(false, 'Wait for the running reply to finish.');
    if (!run.chatId) return;
    openSession(run.chatId);
    ui.chatOpen = true;
  }
</script>

<div>
  {#if run.status !== 'ok'}
    <div class="rounded-lg px-3 py-2.5 {run.status === 'blocked' ? 'bg-warn-soft text-warn' : 'bg-danger-soft text-danger'}">
      {#if run.status === 'blocked'}<p class="label mb-0.5 text-warn">Blocked by the read-only rules</p>{/if}
      <p class="text-sm leading-relaxed break-words">{run.message}</p>
    </div>
  {/if}

  {#if task}
    {#if run.columns && run.rows}
      {@const cols = run.columns}
      {@const rows = run.rows}
      <div class="{run.status !== 'ok' ? 'mt-3 ' : ''}max-h-96 overflow-auto rounded-lg border border-line">
        <table class="w-full text-sm">
          <thead class="sticky top-0 z-sticky bg-surface">
            <tr>{#each cols as c, ci (ci)}<th scope="col" class="th border-b border-line px-3 pt-2 pb-1 whitespace-nowrap {nums[ci] ? 'text-right' : 'text-left'}">{c}</th>{/each}</tr>
          </thead>
          <tbody class="font-mono text-xs">
            {#each rows as r, ri (ri)}
              <tr class="border-t border-line/60">
                {#each cols as _, ci (ci)}
                  {@const v = r[ci]}
                  <td class="max-w-48 truncate px-3 py-1 whitespace-nowrap tabular-nums {nums[ci] ? 'text-right' : 'text-left'}" title={v === null || v === undefined ? 'NULL' : fmt(v)}>
                    {#if v === null || v === undefined}<span class="text-faint italic">NULL</span>{:else}{fmt(v)}{/if}
                  </td>
                {/each}
              </tr>
            {:else}
              <tr><td class="font-sans" colspan={cols.length}><EmptyState title="No rows" /></td></tr>
            {/each}
          </tbody>
        </table>
      </div>
      {#if run.truncated}<p class="mt-1.5 text-xs text-faint">Showing the first {STORED_ROWS} rows.</p>{/if}
    {:else if trimmed}
      <p class="{run.status !== 'ok' ? 'mt-3 ' : ''}text-sm text-faint">Older runs keep only the summary.</p>
    {/if}
    <div class="mt-3">
      <button class="btn btn-secondary" onclick={openEditor} disabled={!conn} title={conn ? 'Open this query in the editor' : 'The connection no longer exists.'}><FileCode size={13} /> Open in editor</button>
    </div>
  {:else}
    {#if run.reply}
      <div class={run.status !== 'ok' ? 'mt-3' : ''}><Markdown text={run.reply} /></div>
    {:else if trimmed}
      <p class="{run.status !== 'ok' ? 'mt-3 ' : ''}text-sm text-faint">Older runs keep only the summary.</p>
    {/if}
    <div class="mt-3">
      <button class="btn btn-secondary" onclick={openChat} disabled={chatMissing} title={chatMissing ? 'This chat was deleted from the chat history.' : 'Open the chat this run wrote'}><MessageSquare size={13} /> Open chat</button>
    </div>
  {/if}
</div>
