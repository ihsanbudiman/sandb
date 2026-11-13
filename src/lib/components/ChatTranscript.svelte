<script lang="ts">
  import { Ban, Braces, Check, ChartColumn, ChevronRight, Clock, Copy, FileCode, FileDown, FileSpreadsheet, LoaderCircle, ListTree, RotateCw, Sparkles, Table2, TriangleAlert, X } from '@lucide/svelte';
  import Markdown from './Markdown.svelte';
  import ChartView from './ChartView.svelte';
  import TargetBadge from './TargetBadge.svelte';
  import { ApiError, type Row } from '../api';
  import { chat, decide, rerun } from '../chat.svelte';
  import type { Msg, ToolPart } from '../chatModel';
  import { destructive, isChartTool, isDescribeTool, isReadTool, mongoStep, mongoTag, startingLabel, STATUS_LABEL, STATUS_TEXT, STATUS_TONE, TOOL_LABEL } from '../chatTools';
  import { afterStep, argsOf, blocks, busy, duration, fenceSource, lastQuery, mongoInReply, numeric, processPart, replyMs, replyText, sourceLabel, stepTarget, toolMs, workEnd, workSteps } from '../chatParts';
  import { connOf, dbOf, type Conn, type Source } from '../connections.svelte';
  import { Flash } from '../flash.svelte';
  import { exportReply } from '../report';
  import { csvCell, formatCell } from '../results';
  import { sqlHtml } from '../highlight';

  let {
    session,
    messages,
    scope,
    scopeConns,
    many,
    showSteps,
    activeSource,
    suggestions,
    oninsert,
    onrun,
    onrunmenu,
    onsubmit,
    onretry,
  }: {
    session: { id: string } | undefined;
    messages: Msg[];
    scope: Source[];
    scopeConns: Conn[];
    many: boolean;
    showSteps: boolean;
    activeSource: Source | undefined;
    suggestions: { text: string; write?: boolean }[];
    oninsert: (sql: string, source?: Source) => void;
    onrun: (sql: string, source?: Source) => void;
    onrunmenu: (sql: string, anchor: HTMLElement, source?: Source) => void;
    /** Sends a new question, from the empty state. */
    onsubmit: (text: string) => void;
    onretry: (mi: number) => void;
  } = $props();

  let list = $state<HTMLDivElement>();
  let stream = $state<HTMLDivElement>();

  // ---- results only ----
  /** Replies whose steps the user opened while steps are hidden. */
  let revealed = $state<Record<string, boolean>>({});
  // Open state the user chose, per step. Without a choice: closed, except while it waits for approval or after it failed.
  let opened = $state<Record<string, boolean>>({});
  const stepKey = (mi: number, at: number) => `${session?.id}:${mi}:${at}`;
  function isOpen(p: ToolPart, key: string) {
    if (p.status === 'awaiting' && !!chat.pending) return true;
    if (key in opened) return opened[key];
    return p.status === 'blocked' || p.status === 'denied' || p.status === 'failed';
  }

  /** Selected cell of a result table, shown in full under it. */
  let cell = $state<{ key: string; col: string; value: unknown } | null>(null);

  const copied = new Flash<string>();
  async function copy(key: string, text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied.show(key);
    } catch (e) {
      console.warn('Copy failed.', e);
    }
  }

  const csv = (rows: Row[]) => {
    const cols = Object.keys(rows[0]);
    return [cols.map(csvCell).join(','), ...rows.map((r) => cols.map((c) => csvCell(r[c])).join(','))].join('\r\n');
  };

  // Puts focus on the safe default of an approval card, so Enter does not run a destructive statement.
  const autofocus = (el: HTMLElement) => el.focus({ preventScroll: true });

  const fmt = (v: unknown) => (typeof v === 'number' ? v.toLocaleString() : v !== null && typeof v === 'object' ? formatCell(v) : String(v));
  /** Full text of a cell for the cell view. */
  const full = (v: unknown) => (v !== null && typeof v === 'object' && !('$bytes' in v) ? JSON.stringify(v, null, 2) : fmt(v));

  // ---- PDF export ----
  let exporting = $state<number | null>(null);
  let exported = $state<{ mi: number; ok: boolean; text: string } | null>(null);
  async function exportPdf(mi: number) {
    const q = messages[mi - 1]?.parts[0];
    const question = q?.kind === 'text' ? q.text.trim() : '';
    exporting = mi;
    exported = null;
    try {
      const meta = [...scope.map(sourceLabel), new Date().toLocaleString()].join(' · ');
      const text = await exportReply(messages[mi].parts, question, meta);
      if (text) exported = { mi, ok: true, text };
    } catch (e) {
      exported = { mi, ok: false, text: `The PDF was not saved: ${ApiError.from(e).message}` };
    } finally {
      exporting = null;
    }
  }

  // Follow the stream only while the reader is at the bottom; scrolling up stops it.
  // A new turn, another chat, or a fresh list (back from history) follows again.
  let follow = true;
  let seen: unknown[] = [];
  function onListScroll() {
    if (list) follow = list.scrollHeight - list.scrollTop - list.clientHeight < 32;
  }

  // Content growth is what a streamed part does; the scroll container's own size does not change.
  $effect(() => {
    const el = stream;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      if (follow) list?.scrollTo({ top: list.scrollHeight });
    });
    ro.observe(el);
    return () => ro.disconnect();
  });

  $effect(() => {
    const key = [list, session?.id, messages.length];
    if (key.some((k, i) => k !== seen[i])) {
      seen = key;
      follow = true;
    }
    if (follow) list?.scrollTo({ top: list.scrollHeight });
  });
</script>

<!-- Text actions of a read: of a step, and of the "Query used" box under a reply whose steps are hidden. -->
{#snippet sqlTools(p: ToolPart, key: string)}
  {@const noun = mongoStep(p) ? 'query' : 'SQL'}
  <button class="grid size-6 place-items-center rounded text-faint hover:bg-hover hover:text-ink" title="Copy {noun}" aria-label="Copy {noun}" onclick={() => copy(`${key}:sql`, p.sql!)}>{#if copied.current === `${key}:sql`}<Check size={13} />{:else}<Copy size={13} />{/if}</button>
  <button class="grid size-6 place-items-center rounded text-faint hover:bg-hover hover:text-ink" title="Put the {noun} in the editor" aria-label="Put the {noun} in the editor" onclick={() => oninsert(p.sql!, p.source)}><FileCode size={13} /></button>
  {#if isReadTool(p) && p.status === 'done'}
    <button class="grid size-6 place-items-center rounded text-faint hover:bg-hover hover:text-ink" title="Open in the results grid (new tab)" aria-label="Open in the results grid" onclick={() => onrun(p.sql!, p.source)}><Table2 size={13} /></button>
  {/if}
{/snippet}

<div bind:this={list} onscroll={onListScroll} class="flex-1 overflow-auto px-4 pt-3 pb-4">
  {#if messages.length === 0}
    <div class="pt-8">
      <div class="grid size-8 place-items-center rounded-lg bg-accent/12 text-accent"><Sparkles size={15} /></div>
      <div class="mt-3 text-[14px] font-semibold">Ask about your data</div>
      <p class="mt-1 text-[12.5px] leading-relaxed text-muted">The assistant can read the schema of the databases below and run read-only queries. Add more databases to ask across them. It cannot change data unless you turn on write mode.</p>
      <div class="mt-5 space-y-1.5">
        {#each suggestions as s}
          <button class="flex w-full items-center rounded-lg border border-line px-3 py-2 text-left text-[12.5px] text-muted hover:bg-hover hover:text-ink" onclick={() => onsubmit(s.text)}>
            {s.text}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  <div bind:this={stream} class="space-y-4">
    {#each messages as m, mi}
      {#if m.role === 'user'}
        <div class="ml-auto w-fit max-w-[85%] rounded-xl rounded-br-sm bg-active px-3 py-2 text-ink">{(m.parts[0] as { text: string }).text}</div>
      {:else}
        {@const work = workSteps(m.parts)}
        {@const end = workEnd(m.parts)}
        {@const compact = !showSteps && !!m.parts.some((x) => x.kind === 'tool')}
        {@const rk = `${session?.id}:${mi}`}
        {@const hide = compact && !revealed[rk]}
        {@const rt = replyMs(m)}
        {@const qt = toolMs(work)}
        <div class="space-y-2.5">
          {#if compact && work.length}
            {@const working = !!chat.live && m === messages.at(-1) && busy(m.parts, !!chat.pending)}
            <button class="-ml-1 flex h-6 items-center gap-1.5 rounded px-1 text-[12px] text-faint hover:bg-hover hover:text-ink" aria-expanded={!hide} onclick={() => (revealed[rk] = !revealed[rk])} title="{hide ? 'Show the steps of this reply' : 'Hide the steps of this reply'}{qt !== undefined ? ` · query ${duration(qt)}` : ''}">
              {#if working}<LoaderCircle size={12} class="animate-spin text-accent" /> Working · step {work.length}
              {:else}<ListTree size={12} /> {work.length} {work.length === 1 ? 'step' : 'steps'}{#if rt !== undefined} · {duration(rt)}{:else if qt !== undefined} · {duration(qt)}{/if}{/if}
              <ChevronRight size={12} class="transition-transform {hide ? '' : 'rotate-90'}" />
            </button>
          {/if}
          {#each blocks(m.parts, hide ? (p, at) => processPart(p, at, end, !!chat.pending) : undefined) as b}
            {#if b.kind === 'part'}
            {@const p = b.p}
            {#if p.kind === 'text'}
              <Markdown text={p.text} mongo={mongoInReply(m, scope)} onrunsql={(sql, anchor, mongo) => onrunmenu(sql, anchor, fenceSource(m, scope, activeSource, mongo))} />
            {:else if p.kind === 'chart'}
              <ChartView part={p} />
            {:else if p.kind === 'error'}
              <p class="flex items-start gap-2 leading-relaxed text-red-700 dark:text-red-400"><TriangleAlert size={13} class="mt-1 shrink-0" /><span class="min-w-0 break-words">{p.text}</span></p>
            {/if}
            {:else}
              {@const running = b.steps.some((s) => s.p.status === 'running')}
              {@const asking = b.steps.some((s) => s.p.status === 'awaiting') && !!chat.pending}
              {@const danger = asking && b.steps.some((s) => s.p.status === 'awaiting' && !!s.p.sql && destructive(s.p))}
              {@const total = b.steps.reduce((n, s) => n + (s.p.ms ?? 0), 0)}
              <div class="overflow-hidden rounded-lg border {asking ? (danger ? 'border-red-500/50 ring-3 ring-red-500/10' : 'border-amber-500/50 ring-3 ring-amber-500/10') : 'border-line'}">
                <div class="flex h-7 items-center gap-2 bg-sunken px-2.5 text-[11.5px] text-faint">
                  {#if running}
                    <LoaderCircle size={12} class="animate-spin text-accent" /> <span>Working · step {b.steps.length}</span>
                  {:else}
                    <span>{b.steps.length} {b.steps.length === 1 ? 'step' : 'steps'}</span>
                  {/if}
                  <span class="ml-auto tabular-nums" title={total ? 'Time the tools of these steps ran' : undefined}>{total ? `query ${duration(total)}` : ''}</span>
                </div>
                <ol>
                  {#each b.steps as { p, at }, si}
                    {@const key = stepKey(mi, at)}
                    {@const open = isOpen(p, key)}
                    {@const waiting = p.status === 'awaiting' && !!chat.pending}
                    {@const risky = waiting && !!p.sql && destructive(p)}
                    {@const implicit = p.info?.warnings.some((w) => w.code === 'implicit_commit') ?? false}
                    {@const refused = p.status === 'blocked' || p.status === 'denied'}
                    {@const failed = refused || p.status === 'failed'}
                    {@const sc = (p.source && connOf(p.source)) || scopeConns[0]}
                    <li class="border-t border-line">
                      <button class="flex h-8 w-full items-center gap-2 px-2.5 text-left text-[12px] hover:bg-hover" aria-expanded={open} onclick={() => (opened[key] = !open)}>
                        <span class="w-3.5 shrink-0 text-center text-[10.5px] text-faint tabular-nums">{si + 1}</span>
                        {#if p.status === 'running'}<LoaderCircle size={13} class="shrink-0 animate-spin text-accent" />
                        {:else if refused}<Ban size={13} class="shrink-0 text-red-600 dark:text-red-400" />
                        {:else if p.status === 'failed'}<TriangleAlert size={13} class="shrink-0 text-red-600 dark:text-red-400" />
                        {:else if p.status === 'awaiting'}<Clock size={13} class="shrink-0 text-amber-600 dark:text-amber-400" />
                        {:else if isChartTool(p)}<ChartColumn size={13} class="shrink-0 text-emerald-600 dark:text-emerald-400" />
                        {:else}<Check size={13} class="shrink-0 text-emerald-600 dark:text-emerald-400" />{/if}
                        <span class="shrink-0 font-medium text-ink">{TOOL_LABEL[p.tool] ?? p.name}</span>
                        {#if many && p.source}<span class="max-w-24 shrink-0 truncate rounded bg-sunken px-1 font-mono text-[10.5px] text-muted" title={sourceLabel(p.source)}>{dbOf(p.source)}</span>{/if}
                        <span class="min-w-0 flex-1 truncate font-mono text-[11.5px] text-muted">{open && !isDescribeTool(p) ? '' : stepTarget(p)}</span>
                        <span class="shrink-0 text-[11px] text-faint tabular-nums">{duration(p.ms)}</span>
                        {#if p.status !== 'done'}<span class="shrink-0 text-[11.5px] {STATUS_TONE[p.status]}" title={STATUS_LABEL[p.status] ?? p.status}><span class="sr-only">{STATUS_LABEL[p.status] ?? p.status}: </span><span class="sentence">{STATUS_TEXT[p.status] ?? p.status}</span></span>{/if}
                        <ChevronRight size={13} class="shrink-0 text-faint transition-transform {open ? 'rotate-90' : ''}" />
                      </button>
                      {#if open}
                        {@const sqlActions = !!p.sql && !isDescribeTool(p) && !waiting}
                        {#if p.sql && !isDescribeTool(p)}
                          <pre class="max-h-64 overflow-auto border-t border-line bg-sunken px-3 py-2 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap text-ink">{@html sqlHtml(p.sql, sc?.engine)}</pre>
                        {/if}
                        {#if p.rows?.length}
                          {@const cols = Object.keys(p.rows[0])}
                          {@const nums = numeric(p.rows)}
                          <div class="max-h-80 overflow-auto border-t border-line">
                            <table class="w-full text-[12px]">
                              <thead class="sticky top-0 bg-surface"><tr>{#each cols as c}<th class="px-3 pt-2 pb-1 text-[11.5px] font-medium whitespace-nowrap text-faint {nums[c] ? 'text-right' : 'text-left'}">{c}</th>{/each}</tr></thead>
                              <tbody class="font-mono text-[11.5px]">
                                {#each p.rows as r, ri}
                                  <tr class="border-t border-line/60">
                                    {#each cols as c}
                                      {@const v = r[c]}
                                      {@const sel = cell?.key === `${key}:${ri}` && cell.col === c}
                                      <td class="max-w-48 p-0">
                                        <button class="block w-full truncate px-3 py-1 whitespace-nowrap tabular-nums {nums[c] ? 'text-right' : 'text-left'} {sel ? 'bg-accent/10' : 'hover:bg-hover'}" title="Show the full value" onclick={() => (cell = sel ? null : { key: `${key}:${ri}`, col: c, value: v })}>
                                          {#if v === null}<span class="text-[11px] text-faint italic">NULL</span>{:else}{fmt(v)}{/if}
                                        </button>
                                      </td>
                                    {/each}
                                  </tr>
                                {/each}
                              </tbody>
                            </table>
                          </div>
                          {#if cell?.key.startsWith(`${key}:`)}
                            {@const c = cell}
                            <div class="border-t border-line bg-sunken px-3 py-2 text-[12px]">
                              <div class="flex items-center gap-2 text-[11.5px] text-faint">
                                <span class="min-w-0 truncate font-mono text-muted">{c.col}</span>
                                <span>row {Number(c.key.slice(key.length + 1)) + 1}</span>
                                <button class="ml-auto grid size-6 place-items-center rounded hover:bg-hover hover:text-ink" title="Copy value" aria-label="Copy value" onclick={() => copy('cell', c.value === null ? '' : full(c.value))}>{#if copied.current === 'cell'}<Check size={12} />{:else}<Copy size={12} />{/if}</button>
                                <button class="grid size-6 place-items-center rounded hover:bg-hover hover:text-ink" title="Close" aria-label="Close value" onclick={() => (cell = null)}><X size={12} /></button>
                              </div>
                              <pre class="mt-1 max-h-40 overflow-auto font-mono text-[11.5px] break-all whitespace-pre-wrap text-ink">{#if c.value === null}<span class="text-faint italic">NULL</span>{:else}{full(c.value)}{/if}</pre>
                            </div>
                          {/if}
                          {#if p.truncated}
                            <div class="border-t border-line px-3 py-1.5 text-[12px] text-amber-700 dark:text-amber-400">More rows exist. The chat shows the first {p.rows.length}.{isReadTool(p) ? ' Open the query in the grid to see all rows.' : ''}</div>
                          {/if}
                        {/if}
                        {#if waiting && p.sql && sc}
                          <div class="space-y-2.5 border-t border-line px-3 py-2.5">
                            <TargetBadge bare conn={sc} name={p.source ? sourceLabel(p.source) : sc.name}>
                              <span class="tag font-mono text-amber-700 dark:text-amber-400">{mongoStep(p) ? mongoTag(p) : p.sql.trim().split(/\s+/)[0].toUpperCase()}</span>
                            </TargetBadge>
                            {#if risky}
                              <div class="flex gap-2 rounded-md bg-red-500/8 px-2.5 py-2 text-[12px] leading-relaxed text-red-700 dark:text-red-300">
                                <TriangleAlert size={13} class="mt-0.5 shrink-0" />
                                <span>{sc.engine === 'mongo' ? 'This statement removes data. You cannot undo it after it runs.' : implicit ? 'This statement removes data. A statement in this batch may commit as it runs, so the batch cannot be fully rolled back.' : 'This statement removes data. It runs inside a transaction, but you cannot undo it after it commits.'}</span>
                              </div>
                            {:else if implicit}
                              <div class="flex gap-2 rounded-md bg-amber-500/10 px-2.5 py-2 text-[12px] leading-relaxed text-amber-800 dark:text-amber-300">
                                <TriangleAlert size={13} class="mt-0.5 shrink-0" />
                                <span>A statement in this batch may commit as it runs, so the batch cannot be fully rolled back.</span>
                              </div>
                            {:else}
                              <p class="text-[12px] text-muted">It runs only if you approve it.</p>
                            {/if}
                            <div class="flex items-center gap-1.5">
                              <button class="flex items-center gap-1 rounded px-1 text-[12px] font-medium text-accent hover:underline" onclick={() => oninsert(p.sql!, p.source)}><Copy size={12} /> To editor</button>
                              {#if risky}
                                <button class="btn btn-secondary ml-auto h-7 px-2.5 text-[12px]" use:autofocus onclick={() => decide(false)}>Deny</button>
                                <button class="btn h-7 bg-red-600 px-2.5 text-[12px] text-white hover:bg-red-500" onclick={() => decide(true)}>Approve and run</button>
                              {:else}
                                <button class="btn btn-secondary ml-auto h-7 px-2.5 text-[12px]" onclick={() => decide(false)}>Deny</button>
                                <button class="btn btn-primary h-7 px-2.5 text-[12px]" use:autofocus onclick={() => decide(true)}>Approve and run</button>
                              {/if}
                            </div>
                          </div>
                        {/if}
                        {#if p.note || sqlActions}
                          <div class="flex items-center gap-2 border-t border-line px-3 py-1 text-[12px] text-muted">
                            <span class="min-w-0 flex-1 {failed ? 'text-red-700 dark:text-red-400' : ''}">{p.note ?? ''}</span>
                            {#if sqlActions}
                              <span class="flex shrink-0 items-center">
                                {@render sqlTools(p, key)}
                                {#if isReadTool(p) && p.status !== 'blocked'}
                                  <button class="grid size-6 place-items-center rounded text-faint hover:bg-hover hover:text-ink disabled:opacity-40" title="Run again" aria-label="Run again" disabled={p.status === 'running'} onclick={() => rerun(p)}><RotateCw size={13} /></button>
                                {/if}
                                {#if p.rows?.length}
                                  <button class="grid size-6 place-items-center rounded text-faint hover:bg-hover hover:text-ink" title="Copy rows as CSV" aria-label="Copy rows as CSV" onclick={() => copy(`${key}:csv`, csv(p.rows!))}>{#if copied.current === `${key}:csv`}<Check size={13} />{:else}<FileSpreadsheet size={13} />{/if}</button>
                                {/if}
                              </span>
                            {/if}
                          </div>
                        {/if}
                        <details class="group border-t border-line text-[11.5px]">
                          <summary class="flex cursor-pointer list-none items-center gap-1.5 px-3 py-1.5 text-faint select-none hover:text-ink [&::-webkit-details-marker]:hidden">
                            <Braces size={12} /> Call details <ChevronRight size={12} class="transition-transform group-open:rotate-90" />
                          </summary>
                          <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 px-3 pb-1.5 font-mono">
                            <dt class="text-faint">tool</dt><dd class="min-w-0 truncate">{p.name}</dd>
                            {#if p.id}<dt class="text-faint">call id</dt><dd class="min-w-0 truncate">{p.id}</dd>{/if}
                            {#if p.source}<dt class="text-faint">database</dt><dd class="min-w-0 truncate">{sourceLabel(p.source)}</dd>{/if}
                            <dt class="text-faint">status</dt><dd>{p.status}</dd>
                            {#if p.ms !== undefined}<dt class="text-faint">duration</dt><dd>{duration(p.ms)}</dd>{/if}
                            {#if p.rows}<dt class="text-faint">rows shown</dt><dd>{p.rows.length}{p.truncated ? ' (more exist)' : ''}</dd>{/if}
                            {#if p.note}<dt class="text-faint">result</dt><dd class="min-w-0 break-words">{p.note}</dd>{/if}
                            {#if p.error}<dt class="text-faint">error</dt><dd class="text-red-700 dark:text-red-400">{p.error}</dd>{/if}
                          </dl>
                          <div class="px-3 pb-2">
                            <div class="font-mono text-faint">arguments</div>
                            <pre class="mt-0.5 max-h-48 overflow-auto rounded bg-sunken px-2 py-1.5 font-mono break-all whitespace-pre-wrap text-muted">{JSON.stringify(argsOf(p), null, 2)}</pre>
                          </div>
                        </details>
                      {/if}
                    </li>
                  {/each}
                </ol>
              </div>
            {/if}
          {/each}
          {#if chat.live && m === messages.at(-1)}
            <!-- Steps show their own state; otherwise the reply waits for the model or streams text. -->
            {#if chat.starting && !busy(m.parts, !!chat.pending)}
              <div class="flex items-center gap-2 text-[12.5px] text-muted"><LoaderCircle size={13} class="animate-spin text-accent" /> {startingLabel(chat.starting.kind, chat.starting.name)}</div>
            {:else if m.parts.length === 0 || afterStep(m.parts, !!chat.pending)}
              <div class="flex items-center gap-2 text-[12.5px] text-muted"><LoaderCircle size={13} class="animate-spin text-accent" /> Thinking…</div>
            {:else if m.parts.at(-1)?.kind === 'text'}
              <div class="flex items-center gap-2 text-[12.5px] text-muted"><LoaderCircle size={13} class="animate-spin text-accent" /> Writing…</div>
            {/if}
          {:else}
            {@const text = replyText(m.parts)}
            {@const printable = !!text || m.parts.some((x) => x.kind === 'chart')}
            {@const q = hide ? lastQuery(m.parts) : undefined}
            {#if q}
              {@const qc = (q.p.source && connOf(q.p.source)) || scopeConns[0]}
              <details class="group overflow-hidden rounded-lg border border-line">
                <summary class="flex h-7 cursor-pointer list-none items-center gap-1.5 bg-sunken px-2.5 text-[11.5px] text-faint select-none hover:text-ink [&::-webkit-details-marker]:hidden">
                  <FileCode size={12} /> Query used
                  {#if many && q.p.source}<span class="max-w-24 shrink-0 truncate rounded bg-surface px-1 font-mono text-[10.5px] text-muted" title={sourceLabel(q.p.source)}>{dbOf(q.p.source)}</span>{/if}
                  <ChevronRight size={12} class="ml-auto transition-transform group-open:rotate-90" />
                </summary>
                <pre class="max-h-64 overflow-auto border-t border-line bg-sunken px-3 py-2 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap text-ink">{@html sqlHtml(q.sql, qc?.engine)}</pre>
                <div class="flex items-center justify-end border-t border-line px-3 py-1 text-muted">
                  <span class="flex shrink-0 items-center">{@render sqlTools(q.p, `${rk}:query`)}</span>
                </div>
              </details>
            {/if}
            <div class="-ml-1 flex items-center text-faint">
              {#if text}
                <button class="grid size-6 place-items-center rounded hover:bg-hover hover:text-ink" title="Copy reply" aria-label="Copy reply" onclick={() => copy(`${mi}:reply`, text)}>{#if copied.current === `${mi}:reply`}<Check size={13} />{:else}<Copy size={13} />{/if}</button>
              {/if}
              {#if printable}
                <button class="flex h-6 items-center gap-1 rounded px-1.5 text-[12px] font-medium hover:bg-hover hover:text-ink disabled:opacity-40" title="Export the reply as a PDF" aria-label="Export as PDF" disabled={exporting !== null} onclick={() => exportPdf(mi)}>{#if exporting === mi}<LoaderCircle size={13} class="animate-spin" />{:else}<FileDown size={13} />{/if} Export PDF</button>
              {/if}
              <button class="grid size-6 place-items-center rounded hover:bg-hover hover:text-ink disabled:opacity-40" title="Ask again" aria-label="Ask again" disabled={!!chat.live} onclick={() => onretry(mi)}><RotateCw size={13} /></button>
              {#if !compact && work.length && rt !== undefined}
                <span class="ml-auto pr-1 text-[11.5px] tabular-nums" title="Reply time{qt !== undefined ? ` · query ${duration(qt)}` : ''}">{duration(rt)}</span>
              {/if}
            </div>
            {#if exported?.mi === mi}
              <p class="text-[12px] break-all {exported.ok ? 'text-muted' : 'text-red-700 dark:text-red-400'}">{exported.text}</p>
            {/if}
          {/if}
        </div>
      {/if}
    {/each}
  </div>
</div>
