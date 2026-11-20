<script lang="ts">
  import { Ban, BookOpen, Braces, Check, ChartColumn, ChevronDown, ChevronRight, Clock, Copy, FileCode, FileDown, FileSpreadsheet, LoaderCircle, ListTree, RotateCw, Sparkles, Table2, TriangleAlert, X } from '@lucide/svelte';
  import Markdown from './Markdown.svelte';
  import TargetBadge from './TargetBadge.svelte';
  import { ApiError, type Row } from '../api';
  import { chat, decide, rerun } from '../chat.svelte';
  import { isActivityPart, lastToolIndex, replyText, type Msg, type ToolPart } from '../chatModel';
  import { destructive, isChartTool, isDescribeTool, isReadTool, mongoStep, mongoTag, redisStep, startingLabel, STATUS_LABEL, STATUS_TEXT, STATUS_TONE, TOOL_LABEL } from '../chatTools';
  import { argsOf, blocks, busy, chatDatabaseLabel, duration, fenceSource, lastQuery, mongoInReply, numeric, replyMs, sourceLabel, stepTarget, toolMs, type Block } from '../chatParts';
  import { connOf, dbOf, type Conn, type Source } from '../connections.svelte';
  import { Flash } from '../flash.svelte';
  import { csvCell, formatCell } from '../results';
  import { skillTitle } from '../skills';
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
    showSteps: boolean | null;
    activeSource: Source | undefined;
    suggestions: { text: string; write?: boolean }[];
    oninsert: (sql: string, source?: Source) => void;
    onrun: (sql: string, source?: Source) => void;
    onrunmenu: (sql: string, anchor: HTMLElement, source?: Source) => void;
    /** Sends a new question, from the empty state. */
    onsubmit: (text: string) => void;
    onretry: (mi: number) => void;
  } = $props();

  const redisEditor = $derived(!!activeSource && connOf(activeSource)?.engine === 'redis');

  let list = $state<HTMLDivElement>();
  let stream = $state<HTMLDivElement>();

  // ---- results only ----
  /** Long questions the user expanded. */
  let revealed = $state<Record<string, boolean>>({});
  let activityOpen = $state<Record<string, boolean>>({});
  $effect(() => {
    showSteps;
    activityOpen = {};
  });
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
      const parts = messages[mi].parts;
      const { exportReply } = await import('../report');
      const text = await exportReply(parts, question, meta);
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
  {@const noun = redisStep(p) ? 'command' : mongoStep(p) ? 'query' : 'SQL'}
  <button class="btn btn-sm btn-ghost size-6 p-0" title="Copy {noun}" aria-label="Copy {noun}" onclick={() => copy(`${key}:sql`, p.sql!)}>{#if copied.current === `${key}:sql`}<Check size={13} />{:else}<Copy size={13} />{/if}</button>
  <button class="btn btn-sm btn-ghost size-6 p-0" title="Put the {noun} in the editor" aria-label="Put the {noun} in the editor" onclick={() => oninsert(p.sql!, p.source)}><FileCode size={13} /></button>
  {#if isReadTool(p) && p.status === 'done'}
    <button class="btn btn-sm btn-ghost size-6 p-0" title="Open in the results grid (new tab)" aria-label="Open in the results grid" onclick={() => onrun(p.sql!, p.source)}><Table2 size={13} /></button>
  {/if}
{/snippet}

<div bind:this={list} onscroll={onListScroll} class="flex-1 overflow-auto px-4 pt-3 pb-4">
  {#if messages.length === 0}
    <div class="pt-8">
      <div class="grid size-8 place-items-center rounded-lg bg-accent/12 text-accent"><Sparkles size={15} /></div>
      <div class="mt-3 text-lg font-semibold">Ask about your data</div>
      <p class="mt-1 text-sm leading-relaxed text-muted">The assistant can read the schema of the databases below and run read-only queries. Add more databases to ask across them. It cannot change data unless you turn on write mode.</p>
      <div class="mt-5 space-y-1.5">
        {#each suggestions as s}
          <button class="focus-ring flex w-full items-center rounded-lg border border-line px-3 py-2 text-left text-sm text-muted hover:bg-hover hover:text-ink" onclick={() => onsubmit(s.text)}>
            {s.text}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  <div bind:this={stream} class="space-y-4">
    {#each messages as m, mi}
      {#if m.role === 'user'}
        {@const text = (m.parts[0] as { text: string }).text}
        {@const uk = `${session?.id}:${mi}:user`}
        {@const long = text.length > 600 || text.split('\n').length > 8}
        <div class="ml-auto w-fit max-w-[85%] rounded-xl rounded-br-sm bg-active px-3 py-2 text-ink">
          {#if m.skills?.length}
            <div class="mb-1 flex flex-wrap justify-end gap-1">
              {#each m.skills as name (name)}
                <span class="chip font-medium" title="Skill sent with this question"><BookOpen size={12} /> {skillTitle(name)}</span>
              {/each}
            </div>
          {/if}
          <div class="break-words whitespace-pre-wrap {long && !revealed[uk] ? 'line-clamp-8' : ''}">{text}</div>
          {#if long}
            <button class="btn btn-sm btn-ghost -ml-1 mt-1 gap-1 px-1" aria-expanded={!!revealed[uk]} onclick={() => (revealed[uk] = !revealed[uk])}>
              <ChevronDown size={12} class="motion-safe:transition-transform motion-safe:duration-150 {revealed[uk] ? 'rotate-180' : ''}" />{revealed[uk] ? 'Show less' : 'Show more'}
            </button>
          {/if}
        </div>
      {:else}
        {@const live = !!chat.live && m === messages.at(-1)}
        {@const waiting = live && !!chat.pending}
        {@const work = m.parts.filter((p): p is ToolPart => p.kind === 'tool')}
        {@const end = live && chat.starting ? m.parts.length : lastToolIndex(m.parts)}
        {@const rk = `${session?.id}:${mi}`}
        {@const expanded = waiting || (activityOpen[rk] ?? showSteps ?? live)}
        {@const rt = replyMs(m)}
        {@const qt = toolMs(work)}
        {@const activity = blocks(m.parts, (p, at) => !isActivityPart(p, at, end))}
        {#snippet transcript(content: Block[])}
          {#each content as b}
            {#if b.kind === 'part'}
            {@const p = b.p}
            {#if p.kind === 'text'}
              <Markdown text={p.text} mongo={mongoInReply(m, scope)} redis={redisEditor} onrunsql={(sql, anchor, language) => {
                const source = fenceSource(m, scope, activeSource, language);
                if (source || !redisEditor) onrunmenu(sql, anchor, source);
              }} />
            {:else if p.kind === 'reasoning'}
              <div class="border-l-2 border-line pl-2.5 text-sm text-muted">
                <div class="text-xs font-medium text-faint">Thinking</div>
                <p class="mt-1 leading-relaxed break-words whitespace-pre-wrap">{p.text}</p>
              </div>
            {:else if p.kind === 'chart'}
              {#await import('./ChartView.svelte')}
                <p class="grid h-72 place-items-center text-muted" role="status">Loading chart…</p>
              {:then { default: ChartView }}
                <ChartView part={p} />
              {:catch e}
                <p class="text-danger" role="alert">Could not load the chart: {ApiError.from(e).message}</p>
              {/await}
            {:else if p.kind === 'error'}
              <p class="flex items-start gap-2 leading-relaxed text-danger"><TriangleAlert size={13} class="mt-1 shrink-0" /><span class="min-w-0 break-words">{p.text}</span></p>
            {/if}
            {:else}
              {@const asking = b.steps.some((s) => s.p.status === 'awaiting') && !!chat.pending}
              {@const danger = asking && b.steps.some((s) => s.p.status === 'awaiting' && !!s.p.sql && destructive(s.p))}
              <div class="overflow-hidden rounded-lg border {asking ? (danger ? 'border-danger/50 ring-3 ring-danger-soft' : 'border-warn/50 ring-3 ring-warn-soft') : 'border-line'}">
                <ol class="divide-y divide-line">
                  {#each b.steps as { p, at }}
                    {@const key = stepKey(mi, at)}
                    {@const open = isOpen(p, key)}
                    {@const waiting = p.status === 'awaiting' && !!chat.pending}
                    {@const risky = waiting && !!p.sql && destructive(p)}
                    {@const implicit = p.info?.warnings.some((w) => w.code === 'implicit_commit') ?? false}
                    {@const refused = p.status === 'blocked' || p.status === 'denied'}
                    {@const tone = STATUS_TONE[p.status]}
                    {@const sc = (p.source && connOf(p.source)) || scopeConns[0]}
                    <li>
                      <button class="focus-ring flex h-rich-menu-row w-full items-center gap-2 bg-sunken px-2.5 text-left text-sm hover:bg-hover focus-visible:outline-offset-[-2px]" aria-expanded={open} onclick={() => (opened[key] = !open)}>
                        <span class="w-3.5 shrink-0 text-center text-2xs text-faint tabular-nums">{work.indexOf(p) + 1}</span>
                        {#if p.status === 'running'}<LoaderCircle size={13} class="shrink-0 motion-safe:animate-spin {tone}" />
                        {:else if refused}<Ban size={13} class="shrink-0 {tone}" />
                        {:else if p.status === 'failed'}<TriangleAlert size={13} class="shrink-0 {tone}" />
                        {:else if p.status === 'awaiting'}<Clock size={13} class="shrink-0 {tone}" />
                        {:else if isChartTool(p)}<ChartColumn size={13} class="shrink-0 {tone}" />
                        {:else if p.tool === 'skill'}<BookOpen size={13} class="shrink-0 text-accent" />
                        {:else}<Check size={13} class="shrink-0 {tone}" />{/if}
                        <span class="shrink-0 font-medium text-ink">{TOOL_LABEL[p.tool] ?? p.name}</span>
                        {#if many && p.source}<span class="max-w-24 shrink-0 truncate rounded bg-sunken px-1 font-mono text-2xs text-muted" title={sourceLabel(p.source)}>{chatDatabaseLabel(connOf(p.source), dbOf(p.source))}</span>{/if}
                        <span class="min-w-0 flex-1 truncate font-mono text-xs text-muted">{open && !isDescribeTool(p) ? '' : stepTarget(p)}</span>
                        <span class="shrink-0 text-xs text-faint tabular-nums">{duration(p.ms)}</span>
                        {#if p.status !== 'done'}<span class="shrink-0 text-xs {tone}" title={STATUS_LABEL[p.status] ?? p.status}><span class="sr-only">{STATUS_LABEL[p.status] ?? p.status}: </span><span class="sentence">{STATUS_TEXT[p.status] ?? p.status}</span></span>{/if}
                        <ChevronRight size={13} class="shrink-0 text-faint motion-safe:transition-transform motion-safe:duration-150 {open ? 'rotate-90' : ''}" />
                      </button>
                      {#if open}
                        {@const sqlActions = !!p.sql && !isDescribeTool(p) && !waiting}
                        {#if p.sql && !isDescribeTool(p)}
                          <pre class="max-h-64 overflow-auto border-t border-line bg-sunken px-3 py-2 font-mono text-xs leading-relaxed whitespace-pre-wrap text-ink">{@html sqlHtml(p.sql, redisStep(p) ? 'redis' : sc?.engine)}</pre>
                        {/if}
                        {#if p.rows?.length}
                          {@const cols = Object.keys(p.rows[0])}
                          {@const nums = numeric(p.rows)}
                          <div class="max-h-80 overflow-auto border-t border-line">
                            <table class="w-full text-sm">
                              <thead class="sticky top-0 z-sticky bg-surface"><tr>{#each cols as c}<th scope="col" class="th px-3 pt-2 pb-1 whitespace-nowrap {nums[c] ? 'text-right' : 'text-left'}">{c}</th>{/each}</tr></thead>
                              <tbody class="font-mono text-xs">
                                {#each p.rows as r, ri}
                                  <tr class="border-t border-line/60">
                                    {#each cols as c}
                                      {@const v = r[c]}
                                      {@const sel = cell?.key === `${key}:${ri}` && cell.col === c}
                                      <td class="max-w-48 p-0">
                                        <button class="focus-ring block w-full truncate px-3 py-1 whitespace-nowrap tabular-nums focus-visible:outline-offset-[-2px] {nums[c] ? 'text-right' : 'text-left'} {sel ? 'bg-accent/10' : 'hover:bg-hover'}" title="Show the full value" onclick={() => (cell = sel ? null : { key: `${key}:${ri}`, col: c, value: v })}>
                                          {#if v === null}<span class="text-xs text-faint italic">NULL</span>{:else}{fmt(v)}{/if}
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
                            <div class="border-t border-line bg-sunken px-3 py-2 text-sm">
                              <div class="flex items-center gap-2 text-xs text-faint">
                                <span class="min-w-0 truncate font-mono text-muted">{c.col}</span>
                                <span>row {Number(c.key.slice(key.length + 1)) + 1}</span>
                                <button class="btn btn-sm btn-ghost ml-auto size-6 p-0" title="Copy value" aria-label="Copy value" onclick={() => copy('cell', c.value === null ? '' : full(c.value))}>{#if copied.current === 'cell'}<Check size={12} />{:else}<Copy size={12} />{/if}</button>
                                <button class="btn btn-sm btn-ghost size-6 p-0" title="Close" aria-label="Close value" onclick={() => (cell = null)}><X size={12} /></button>
                              </div>
                              <pre class="mt-1 max-h-40 overflow-auto font-mono text-xs break-all whitespace-pre-wrap text-ink">{#if c.value === null}<span class="text-faint italic">NULL</span>{:else}{full(c.value)}{/if}</pre>
                            </div>
                          {/if}
                          {#if p.truncated}
                            <div class="border-t border-line px-3 py-1.5 text-sm text-warn">More rows exist. The chat shows the first {p.rows.length}.{isReadTool(p) ? ' Open the query in the grid to see all rows.' : ''}</div>
                          {/if}
                        {/if}
                        {#if waiting && p.sql && sc}
                          <div class="space-y-2.5 border-t border-line px-3 py-2.5">
                            <TargetBadge bare conn={sc} name={p.source ? sourceLabel(p.source) : sc.name}>
                              <span class="tag font-mono text-warn">{mongoStep(p) ? mongoTag(p) : (sc.engine === 'redis' && p.info?.statements[0]?.kind) || p.sql.trim().split(/\s+/, 1)[0].toUpperCase()}</span>
                            </TargetBadge>
                            {#if sc.engine === 'redis'}
                              <p class="text-sm {risky ? 'text-danger' : 'text-muted'}">Redis runs this as one command. There is no rollback after it ran.</p>
                              {#each p.info?.warnings ?? [] as w}
                                <div class="flex gap-2 rounded-md bg-danger-soft px-2.5 py-2 text-sm leading-relaxed text-danger">
                                  <TriangleAlert size={13} class="mt-0.5 shrink-0" />
                                  <span>{w.message}</span>
                                </div>
                              {/each}
                            {:else if risky}
                              <div class="flex gap-2 rounded-md bg-danger-soft px-2.5 py-2 text-sm leading-relaxed text-danger">
                                <TriangleAlert size={13} class="mt-0.5 shrink-0" />
                                <span>{sc.engine === 'mongo' ? 'This statement removes data. You cannot undo it after it runs.' : implicit ? 'This statement removes data. A statement in this batch may commit as it runs, so the batch cannot be fully rolled back.' : 'This statement removes data. It runs inside a transaction, but you cannot undo it after it commits.'}</span>
                              </div>
                            {:else if implicit}
                              <div class="flex gap-2 rounded-md bg-warn-soft px-2.5 py-2 text-sm leading-relaxed text-warn">
                                <TriangleAlert size={13} class="mt-0.5 shrink-0" />
                                <span>A statement in this batch may commit as it runs, so the batch cannot be fully rolled back.</span>
                              </div>
                            {:else}
                              <p class="text-sm text-muted">It runs only if you approve it.</p>
                            {/if}
                            <div class="flex flex-wrap items-center gap-1.5">
                              <button class="btn btn-sm btn-ghost px-1 text-accent" onclick={() => oninsert(p.sql!, p.source)}><Copy size={12} /> To editor</button>
                              {#if risky}
                                <button class="btn btn-sm btn-secondary ml-auto" use:autofocus onclick={() => decide(false)}>Deny</button>
                                <button class="btn btn-sm btn-danger" onclick={() => decide(true)}>Approve and run</button>
                              {:else}
                                <button class="btn btn-sm btn-secondary ml-auto" onclick={() => decide(false)}>Deny</button>
                                <button class="btn btn-sm btn-primary" use:autofocus onclick={() => decide(true)}>Approve and run</button>
                              {/if}
                            </div>
                          </div>
                        {/if}
                        {#if p.note || sqlActions}
                          <div class="flex items-center gap-2 border-t border-line px-3 py-1 text-sm text-muted">
                            <span class="min-w-0 flex-1 {refused || p.status === 'failed' ? tone : ''}">{p.note ?? ''}</span>
                            {#if sqlActions}
                              <span class="flex shrink-0 items-center">
                                {@render sqlTools(p, key)}
                                {#if isReadTool(p) && p.status !== 'blocked'}
                                  <button class="btn btn-sm btn-ghost size-6 p-0" title="Run again" aria-label="Run again" disabled={p.status === 'running'} onclick={() => rerun(p)}><RotateCw size={13} /></button>
                                {/if}
                                {#if p.rows?.length}
                                  <button class="btn btn-sm btn-ghost size-6 p-0" title="Copy rows as CSV" aria-label="Copy rows as CSV" onclick={() => copy(`${key}:csv`, csv(p.rows!))}>{#if copied.current === `${key}:csv`}<Check size={13} />{:else}<FileSpreadsheet size={13} />{/if}</button>
                                {/if}
                              </span>
                            {/if}
                          </div>
                        {/if}
                        <details class="group border-t border-line text-xs">
                          <summary class="focus-ring flex cursor-pointer list-none items-center gap-1.5 px-3 py-1.5 text-faint select-none hover:text-ink focus-visible:outline-offset-[-2px] [&::-webkit-details-marker]:hidden">
                            <Braces size={12} /> Call details <ChevronRight size={12} class="group-open:rotate-90 motion-safe:transition-transform motion-safe:duration-150" />
                          </summary>
                          <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 px-3 pb-1.5 font-mono">
                            <dt class="text-faint">tool</dt><dd class="min-w-0 truncate">{p.name}</dd>
                            {#if p.id}<dt class="text-faint">call id</dt><dd class="min-w-0 truncate">{p.id}</dd>{/if}
                            {#if p.source}<dt class="text-faint">database</dt><dd class="min-w-0 truncate">{sourceLabel(p.source)}</dd>{/if}
                            <dt class="text-faint">status</dt><dd>{p.status}</dd>
                            {#if p.ms !== undefined}<dt class="text-faint">duration</dt><dd>{duration(p.ms)}</dd>{/if}
                            {#if p.rows}<dt class="text-faint">rows shown</dt><dd>{p.rows.length}{p.truncated ? ' (more exist)' : ''}</dd>{/if}
                            {#if p.note}<dt class="text-faint">result</dt><dd class="min-w-0 break-words">{p.note}</dd>{/if}
                            {#if p.error}<dt class="text-faint">error</dt><dd class="text-danger">{p.error}</dd>{/if}
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
        {/snippet}
        <div class="space-y-2.5">
          {#if activity.length}
            <div class="overflow-hidden rounded-lg border border-line">
              <button class="focus-ring flex h-rich-menu-row w-full items-center gap-2 px-2.5 text-left text-sm text-muted hover:bg-hover focus-visible:outline-offset-[-2px]" aria-expanded={expanded} disabled={waiting} onclick={() => (activityOpen[rk] = !expanded)}>
                {#if live && !waiting}<LoaderCircle size={13} class="shrink-0 text-accent motion-safe:animate-spin" />
                {:else}<ListTree size={13} class="shrink-0" />{/if}
                <span class="font-medium text-ink">Activity</span>
                {#if work.length}<span>{work.length} {work.length === 1 ? 'step' : 'steps'}</span>{/if}
                <span class="ml-auto text-xs text-faint tabular-nums" title={qt !== undefined ? `Tool time ${duration(qt)}` : undefined}>{duration(rt ?? qt)}</span>
                <ChevronRight size={13} class="shrink-0 text-faint motion-safe:transition-transform motion-safe:duration-150 {expanded ? 'rotate-90' : ''}" />
              </button>
              {#if expanded}
                <div class="space-y-2.5 border-t border-line p-2.5">
                  {@render transcript(activity)}
                </div>
              {/if}
            </div>
          {/if}
          {@render transcript(blocks(m.parts, (p, at) => isActivityPart(p, at, end)))}
          {#if live}
            <!-- Steps show their own state; otherwise the reply waits for the model or streams text. -->
            {#if chat.starting && !busy(m.parts, !!chat.pending)}
              <div class="flex items-center gap-2 text-sm text-muted"><LoaderCircle size={13} class="text-accent motion-safe:animate-spin" /> {startingLabel(chat.starting.kind, chat.starting.name)}</div>
            {:else if !busy(m.parts, waiting) && m.parts.at(-1)?.kind !== 'error' && m.parts.at(-1)?.kind !== 'text'}
              <div class="flex items-center gap-2 text-sm text-muted"><LoaderCircle size={13} class="text-accent motion-safe:animate-spin" /> Thinking…</div>
            {:else if m.parts.at(-1)?.kind === 'text'}
              <div class="flex items-center gap-2 text-sm text-muted"><LoaderCircle size={13} class="text-accent motion-safe:animate-spin" /> Writing…</div>
            {/if}
          {:else}
            {@const text = replyText(m.parts)}
            {@const printable = !!text || m.parts.some((x) => x.kind === 'chart')}
            {@const q = !expanded ? lastQuery(m.parts) : undefined}
            {#if q}
              {@const qc = (q.p.source && connOf(q.p.source)) || scopeConns[0]}
              <details class="group overflow-hidden rounded-lg border border-line">
                <summary class="focus-ring flex h-list-row cursor-pointer list-none items-center gap-1.5 bg-sunken px-2.5 text-xs text-faint select-none hover:text-ink focus-visible:outline-offset-[-2px] [&::-webkit-details-marker]:hidden">
                  <FileCode size={12} /> {redisStep(q.p) ? 'Command used' : 'Query used'}
                  {#if many && q.p.source}<span class="max-w-24 shrink-0 truncate rounded bg-surface px-1 font-mono text-2xs text-muted" title={sourceLabel(q.p.source)}>{chatDatabaseLabel(connOf(q.p.source), dbOf(q.p.source))}</span>{/if}
                  <ChevronRight size={12} class="ml-auto group-open:rotate-90 motion-safe:transition-transform motion-safe:duration-150" />
                </summary>
                <pre class="max-h-64 overflow-auto border-t border-line bg-sunken px-3 py-2 font-mono text-xs leading-relaxed whitespace-pre-wrap text-ink">{@html sqlHtml(q.sql, redisStep(q.p) ? 'redis' : qc?.engine)}</pre>
                <div class="flex items-center justify-end border-t border-line px-3 py-1 text-muted">
                  <span class="flex shrink-0 items-center">{@render sqlTools(q.p, `${rk}:query`)}</span>
                </div>
              </details>
            {/if}
            <div class="-ml-1 flex items-center text-faint">
              {#if text}
                <button class="btn btn-sm btn-ghost size-6 p-0" title="Copy reply" aria-label="Copy reply" onclick={() => copy(`${mi}:reply`, text)}>{#if copied.current === `${mi}:reply`}<Check size={13} />{:else}<Copy size={13} />{/if}</button>
              {/if}
              {#if printable}
                <button class="btn btn-sm btn-ghost gap-1 px-1.5" title="Export the reply as a PDF" aria-label="Export as PDF" disabled={exporting !== null} onclick={() => exportPdf(mi)}>{#if exporting === mi}<LoaderCircle size={13} class="motion-safe:animate-spin" />{:else}<FileDown size={13} />{/if} Export PDF</button>
              {/if}
              <button class="btn btn-sm btn-ghost size-6 p-0" title="Ask again" aria-label="Ask again" disabled={!!chat.live} onclick={() => onretry(mi)}><RotateCw size={13} /></button>
            </div>
            {#if exported?.mi === mi}
              <p class="text-sm break-all {exported.ok ? 'text-muted' : 'text-danger'}">{exported.text}</p>
            {/if}
          {/if}
        </div>
      {/if}
    {/each}
  </div>
</div>
