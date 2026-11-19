<script lang="ts">
  import { CalendarClock, CalendarPlus, ChevronLeft, ChevronRight, CopyPlus, Database, Ellipsis, LoaderCircle, Pencil, Play, Sparkles, Square, Trash } from '@lucide/svelte';
  import { tick } from 'svelte';
  import ScheduleRunOutput from './ScheduleRunOutput.svelte';
  import TargetBadge from './TargetBadge.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import EngineBadge from './ui/EngineBadge.svelte';
  import Menu from './ui/Menu.svelte';
  import PageHeader from './ui/PageHeader.svelte';
  import Switch from './ui/Switch.svelte';
  import { confirm } from '../ask.svelte';
  import { ago, duration, sourceLabel } from '../chatParts';
  import { describeCadence, formatWhen } from '../cadence';
  import { connOf, db, defaultDb, engines, envDot, type Conn } from '../connections.svelte';
  import { sqlHtml } from '../highlight';
  import {
    deleteSchedule,
    duplicateSchedule,
    editSchedule,
    findSchedule,
    newSchedule,
    openSchedule,
    runNow,
    schedules,
    setEnabled,
    stopRun,
    type Notify,
    type Schedule,
  } from '../schedules.svelte';
  import { skillTitle } from '../skills';
  import { ui } from '../state.svelte';

  const pageId = $props.id();
  let pick = $state<string | null>(null);
  let menuOpen = $state(false);
  let menuTrigger = $state<HTMLButtonElement>();
  let allSchedulesButton = $state<HTMLButtonElement>();
  let elapsed = $state(0);

  /** The open detail; the overview shows when the selection is empty or the schedule is gone. */
  const sel = $derived(schedules.selected ? findSchedule(schedules.selected) : undefined);
  /** The clicked run, else the newest. */
  const selRun = $derived(pick ? (sel?.runs.find((r) => r.id === pick) ?? sel?.runs[0]) : sel?.runs[0]);
  const running = $derived(!!sel && schedules.running?.id === sel.id);
  const queued = $derived(!!sel && schedules.queue.some((q) => q.id === sel.id));

  // The running row's seconds tick once a second while the open schedule runs.
  $effect(() => {
    if (!running) {
      elapsed = 0;
      return;
    }
    const started = schedules.running!.started;
    elapsed = Math.max(0, Math.floor((Date.now() - started) / 1000));
    const t = setInterval(() => (elapsed = Math.max(0, Math.floor((Date.now() - started) / 1000))), 1000);
    return () => clearInterval(t);
  });

  const STATUS = {
    ok: { dot: 'bg-ok', text: 'text-muted' },
    error: { dot: 'bg-danger', text: 'text-danger' },
    blocked: { dot: 'bg-warn', text: 'text-warn' },
  };
  const TRIGGER: Record<string, string> = { schedule: 'Scheduled', 'catch-up': 'Missed run', manual: 'Manual' };
  const NOTIFY: Record<Notify, string> = { always: 'Notify always', failure: 'Notify on failure', never: 'Never notify' };

  const lastRun = (s: Schedule) => s.runs[0];
  const connOfTask = (s: Schedule): Conn | undefined => {
    const t = s.task;
    return t.kind === 'query' ? db.connections.find((c) => c.id === t.connId) : undefined;
  };
  const targetLabel = (s: Schedule, c: Conn) => {
    const t = s.task;
    return t.kind === 'query' ? (c.engine === 'sqlite' ? c.name : `${c.name} / ${t.database || defaultDb(c)}`) : c.name;
  };

  const scheduleButtonId = (id: string) => `${pageId}-schedule-${id}`;

  async function showSchedule(id: string) {
    openSchedule(id);
    await tick();
    allSchedulesButton?.focus();
  }

  async function showAllSchedules(id: string) {
    openSchedule(null);
    await tick();
    document.getElementById(scheduleButtonId(id))?.focus();
  }

  async function remove(s: Schedule) {
    if (!await confirm({ title: 'Delete this schedule?', body: s.name, confirmLabel: 'Delete', destructive: true })) return;
    deleteSchedule(s.id);
  }
</script>

<div class="flex h-full flex-col">
  <PageHeader title="Schedules" subtitle="Run while sandb is open" onback={() => (ui.page = 'query')}>
    {#snippet actions()}
      {#if schedules.list.length > 0}
        <button class="btn btn-primary" onclick={() => void newSchedule()}><CalendarPlus size={13} /> New schedule</button>
      {/if}
    {/snippet}
  </PageHeader>

  <div class="flex-1 overflow-auto">
    {#if sel}
      {@const s = sel}
      {@const conn = connOfTask(s)}
      <!-- Detail -->
      <div class="mx-auto max-w-[56rem] space-y-4 px-6 py-4">
        <button type="button" class="btn btn-sm btn-ghost -ml-2" bind:this={allSchedulesButton} onclick={() => void showAllSchedules(s.id)}><ChevronLeft size={12} /> All schedules</button>

        <div class="card p-4">
          <div class="flex items-center gap-2.5">
            {#if s.task.kind === 'query'}<Database size={15} class="shrink-0 text-faint" />{:else}<Sparkles size={15} class="shrink-0 text-faint" />{/if}
            <h2 class="min-w-0 truncate text-lg font-semibold">{s.name}</h2>
            {#if running}
              <span class="flex shrink-0 items-center gap-1.5 text-xs font-medium text-accent"><LoaderCircle size={12} class="motion-safe:animate-spin" /> Running… {elapsed} s</span>
            {/if}
            <div class="flex-1"></div>
            <button
              class="btn btn-secondary"
              onclick={() => runNow(s.id)}
              disabled={running || queued}
              title={running || queued ? 'Already running' : 'Run it once now'}><Play size={13} /> Run now</button>
            {#if running && schedules.running?.stop}
              <button class="btn btn-secondary" onclick={() => stopRun()}><Square size={13} /> Stop</button>
            {/if}
            <button class="btn btn-secondary" onclick={() => void editSchedule(s.id)}><Pencil size={13} /> Edit</button>
            <button
              type="button"
              class="btn btn-ghost size-7 p-0"
              bind:this={menuTrigger}
              onclick={() => (menuOpen = !menuOpen)}
              aria-label="More actions"
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              aria-controls={menuOpen ? `${pageId}-actions` : undefined}><Ellipsis size={13} /></button>
            {#if menuOpen && menuTrigger}
              <Menu anchor={menuTrigger} align="end" id={`${pageId}-actions`} label="Schedule actions" onclose={() => (menuOpen = false)}>
                <button type="button" class="menu-item" role="menuitem" tabindex="-1" onclick={() => duplicateSchedule(s.id)}><CopyPlus size={13} /> Duplicate</button>
                <div role="separator" class="mx-2 my-1 h-px bg-line"></div>
                <button type="button" class="menu-item nav-item-danger" role="menuitem" tabindex="-1" onclick={() => void remove(s)}><Trash size={13} /> Delete</button>
              </Menu>
            {/if}
            <Switch checked={s.enabled} aria-label="Schedule active" title={s.enabled ? 'Active' : 'Paused'} onchange={(enabled) => setEnabled(s.id, enabled)} />
          </div>
          <div class="mt-3 flex flex-wrap items-center gap-1.5">
            <span class="tag"><CalendarClock size={12} /> {describeCadence(s.cadence)}</span>
            <span class="tag {s.enabled ? '' : 'text-warn'}">{s.enabled ? `Next ${formatWhen(s.nextAt)}` : 'Paused'}</span>
            {#if s.task.kind === 'query'}
              <span class="tag {s.task.allowWrites ? (conn?.env === 'production' ? 'text-danger' : 'text-warn') : ''}">{s.task.allowWrites ? 'Writes allowed' : 'Read-only'}</span>
            {:else}
              <span class="tag">Read-only</span>
            {/if}
            <span class="tag">{NOTIFY[s.notify]}</span>
          </div>
        </div>

        <section>
          <h3 class="label mb-1.5">Task</h3>
          {#if s.task.kind === 'query'}
            <div class="card p-3.5">
              {#if conn}
                <TargetBadge bare {conn} name={targetLabel(s, conn)} />
              {:else}
                <p class="text-sm text-danger">The connection no longer exists.</p>
              {/if}
              <pre class="mt-2 max-h-64 overflow-auto rounded-md bg-sunken px-3 py-2 font-mono text-xs leading-relaxed whitespace-pre-wrap text-ink">{@html sqlHtml(s.task.sql, conn?.engine)}</pre>
            </div>
          {:else}
            <div class="card p-3.5">
              <div class="flex flex-wrap items-center gap-1.5">
                {#each s.task.sources as src, i (i)}
                  {@const c = connOf(src)}
                  <span class="tag" title={sourceLabel(src)}>{#if c}{c.name}{:else}Deleted connection{/if}</span>
                {/each}
                {#each s.task.skills as k (k)}
                  <span class="tag text-accent">{skillTitle(k)}</span>
                {/each}
              </div>
              <pre class="mt-2 max-h-64 overflow-auto rounded-md bg-sunken px-3 py-2 text-sm leading-relaxed whitespace-pre-wrap text-ink">{s.task.prompt}</pre>
            </div>
          {/if}
        </section>

        <section>
          <h3 class="label mb-1.5">Runs</h3>
          <div class="card overflow-hidden">
            {#if running}
              <div class="flex items-center gap-2.5 px-3 py-2 text-sm text-muted">
                <LoaderCircle size={12} class="shrink-0 text-accent motion-safe:animate-spin" />
                <span class="w-[104px] shrink-0 tabular-nums">{formatWhen(schedules.running!.started)}</span>
                <span class="min-w-0 flex-1 truncate">Running…</span>
                <span class="shrink-0 text-right text-xs text-faint tabular-nums">{elapsed} s</span>
              </div>
            {/if}
            {#each s.runs as r, ri (r.id)}
              {@const on = selRun?.id === r.id}
              <button type="button" class="focus-ring flex w-full cursor-pointer items-center gap-2.5 px-3 py-2 text-left text-sm focus-visible:-outline-offset-2 {on ? 'bg-hover' : 'hover:bg-hover'} {running || ri > 0 ? 'border-t border-line/60' : ''}" aria-pressed={on} onclick={() => (pick = r.id)}>
                <span class="size-1.5 shrink-0 rounded-full {STATUS[r.status].dot}" aria-hidden="true"></span>
                <span class="sr-only">{r.status}: </span>
                <span class="w-[104px] shrink-0 text-faint tabular-nums">{formatWhen(r.started)}</span>
                <span class="tag shrink-0">{TRIGGER[r.trigger]}</span>
                <span class="min-w-0 flex-1 truncate {STATUS[r.status].text}" title={r.message}>{r.message}</span>
                <span class="shrink-0 text-right text-xs text-faint tabular-nums">{duration(r.ended - r.started)}</span>
              </button>
            {/each}
            {#if !running && s.runs.length === 0}
              <EmptyState title="No runs yet" hint="Run now to try it." />
            {/if}
            {#if selRun}
              <div class="border-t border-line px-3.5 py-3.5"><ScheduleRunOutput run={selRun} schedule={s} /></div>
            {/if}
          </div>
        </section>
      </div>
    {:else if schedules.list.length === 0}
      <EmptyState class="mx-auto h-full max-w-xs justify-center" title="No schedules" hint="Run a query or ask the assistant on a timer. Schedules run while sandb is open.">
        {#snippet icon()}<CalendarClock size={15} />{/snippet}
        {#snippet action()}
          <button class="btn btn-primary" onclick={() => void newSchedule()}><CalendarPlus size={13} /> New schedule</button>
        {/snippet}
      </EmptyState>
    {:else}
      <!-- Overview -->
      <div class="@container mx-auto max-w-[56rem] px-6 py-4">
        <div class="overflow-hidden rounded-lg border border-line bg-surface">
          <table class="w-full table-fixed border-separate border-spacing-0 text-sm">
            <thead class="bg-sunken">
              <tr class="text-left [&>th]:border-b [&>th]:border-line">
                <th scope="col" class="th w-[27%] truncate py-2 pr-2 pl-3 @[46rem]:w-[21%]">Name</th>
                <th scope="col" class="th truncate px-2 py-2">Target</th>
                <th scope="col" class="th hidden w-[152px] truncate px-2 py-2 @[31rem]:table-cell">Schedule</th>
                <th scope="col" class="th hidden w-[120px] truncate px-2 py-2 @[46rem]:table-cell">Next run</th>
                <th scope="col" class="th w-[88px] truncate px-2 py-2">Last run</th>
                <th scope="col" class="th w-[56px] truncate py-2 pr-3 pl-2 text-right">Active</th>
              </tr>
            </thead>
            <tbody class="[&>tr+tr>td]:border-t [&>tr+tr>td]:border-line/60">
              {#each schedules.list as s (s.id)}
                {@const last = lastRun(s)}
                {@const conn = connOfTask(s)}
                {@const cadence = describeCadence(s.cadence)}
                {@const next = s.enabled ? formatWhen(s.nextAt) : 'Paused'}
                {@const lastTime = last ? ago(last.ended) : ''}
                <tr class="hover:bg-hover focus-within:bg-hover">
                  <td class="h-sub-toolbar pr-2 pl-3 align-middle">
                    <button type="button" id={scheduleButtonId(s.id)} class="focus-ring group flex h-list-row w-full cursor-pointer items-center gap-1.5 rounded text-left font-medium" title={s.name} onclick={() => void showSchedule(s.id)}>
                      {#if s.task.kind === 'query'}<Database size={12} class="shrink-0 text-faint" />{:else}<Sparkles size={12} class="shrink-0 text-faint" />{/if}
                      <span class="min-w-0 flex-1 truncate">{s.name}</span>
                      <ChevronRight size={12} class="shrink-0 text-faint group-hover:text-ink" />
                    </button>
                  </td>
                  <td class="px-2 align-middle">
                    {#if s.task.kind === 'query'}
                      {#if conn}
                        {@const engine = engines[conn.engine]}
                        {@const database = conn.engine === 'sqlite' ? '' : s.task.database || defaultDb(conn)}
                        <div class="flex items-center gap-1.5" title={`${engine.label} · ${conn.name}${database ? ` / ${database}` : ''} · ${conn.env}`}>
                          <EngineBadge engine={conn.engine} />
                          <span class="truncate font-medium">{conn.name}{#if database}<span class="ml-1 font-mono font-normal text-muted">/ {database}</span>{/if}</span>
                          <span class="size-1.5 shrink-0 rounded-full {envDot[conn.env]}" aria-hidden="true"></span>
                          <span class="sr-only">, {conn.env}</span>
                        </div>
                      {:else}
                        <span class="block truncate text-faint italic" title="Deleted connection">Deleted connection</span>
                      {/if}
                    {:else}
                      {@const sources = s.task.sources.map(sourceLabel)}
                      <span class="block truncate text-muted" title={sources.join(', ')}>{sources.length === 1 ? sources[0] : `${sources.length} databases`}</span>
                    {/if}
                  </td>
                  <td class="hidden truncate px-2 align-middle text-muted tabular-nums @[31rem]:table-cell" title={cadence}>{cadence}</td>
                  <td class="hidden truncate px-2 align-middle text-muted tabular-nums @[46rem]:table-cell" title={next}>{next}</td>
                  <td class="px-2 align-middle tabular-nums" title={last ? `${lastTime} · ${last.message}` : 'No runs yet'}>
                    {#if last}
                      <span class="flex items-center gap-1.5">
                        <span class="size-1.5 shrink-0 rounded-full {STATUS[last.status].dot}" aria-hidden="true"></span>
                        <span class="sr-only">{last.status}: </span>
                        <span class="truncate {STATUS[last.status].text}">{lastTime}</span>
                      </span>
                    {:else}
                      <span class="text-faint">—</span>
                    {/if}
                  </td>
                  <td class="pr-3 pl-2 text-right align-middle">
                    <Switch size="sm" class="align-middle" checked={s.enabled} aria-label="{s.name} active" title={s.enabled ? 'Active' : 'Paused'} onchange={(enabled) => setEnabled(s.id, enabled)} />
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </div>
    {/if}
  </div>
</div>
