<script lang="ts">
  import { Search, Copy, Check, ChevronRight } from '@lucide/svelte';
  import { environmentLabel, db, envDot } from '../connections.svelte';
  import { ui } from '../state.svelte';
  import { audit, type AuditEntry } from '../audit.svelte';
  import { Flash } from '../flash.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import PageHeader from './ui/PageHeader.svelte';
  import Select from './ui/Select.svelte';

  type Tab = 'all' | 'read' | 'write' | 'blocked';

  const pageId = $props.id();

  let tab = $state<Tab>('all');
  let q = $state('');
  let connId = $state('all');
  let openId = $state<number | null>(null);
  const copied = new Flash<number>();

  const conns = $derived(Object.fromEntries(db.connections.map((c) => [c.id, c])));
  const connectionOptions = $derived([
    { value: 'all', label: 'All connections' },
    ...db.connections.map((c) => ({ value: c.id, label: c.name })),
  ]);
  const isBlocked = (e: AuditEntry) => e.result.status === 'blocked' || e.result.status === 'denied';

  const base = $derived.by(() => {
    const s = q.trim().toLowerCase();
    return audit.entries
      .filter((e) => connId === 'all' || e.connId === connId)
      .filter((e) => !s || e.sql.toLowerCase().includes(s) || (conns[e.connId]?.name ?? e.connId).toLowerCase().includes(s))
      .sort((a, b) => b.time.getTime() - a.time.getTime());
  });

  const tabs = $derived([
    { id: 'all' as Tab, label: 'All', n: base.length },
    { id: 'read' as Tab, label: 'Reads', n: base.filter((e) => e.kind === 'READ').length },
    { id: 'write' as Tab, label: 'Writes', n: base.filter((e) => e.kind === 'WRITE').length },
    { id: 'blocked' as Tab, label: 'Blocked', n: base.filter(isBlocked).length },
  ]);

  const rows = $derived(
    base.filter((e) => tab === 'all' || (tab === 'read' ? e.kind === 'READ' : tab === 'write' ? e.kind === 'WRITE' : isBlocked(e))),
  );

  function fmtTime(d: Date) {
    const today = new Date().toDateString() === d.toDateString();
    const t = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
    return today ? t : `${d.toLocaleDateString([], { month: 'short', day: 'numeric' })} ${t}`;
  }

  function resultText(e: AuditEntry) {
    const r = e.result;
    if (r.status === 'ok') return e.kind === 'READ' ? `${r.rows} ${r.rows === 1 ? 'row' : 'rows'}` : `${r.rows} affected`;
    if (r.status === 'error') return `Error: ${r.message}`;
    return r.status === 'blocked' ? 'Blocked (read-only)' : 'Denied by you';
  }

  async function copy(e: AuditEntry) {
    try {
      await navigator.clipboard.writeText(e.sql);
      copied.show(e.id);
    } catch (err) {
      console.warn('Copy failed.', err);
    }
  }
</script>

<div class="@container flex h-full flex-col">
  <PageHeader class="[@media(max-height:400px)]:h-8" title="Audit log" subtitle="Every query run by you or the AI" onback={() => (ui.page = 'query')} />

  <div class="grid shrink-0 grid-cols-[minmax(0,1fr)_9rem] items-center gap-2 px-4 pb-2 @3xl:flex">
    <div class="col-span-2 overflow-x-auto @3xl:shrink-0">
      <div class="seg whitespace-nowrap">
        {#each tabs as t (t.id)}
          <button
            class="seg-item flex items-center gap-1.5 {tab === t.id ? 'seg-on' : ''}"
            aria-pressed={tab === t.id}
            onclick={() => (tab = t.id)}>{t.label} <span class="text-xs font-normal text-faint tabular-nums">{t.n}</span></button>
        {/each}
      </div>
    </div>
    <div class="hidden flex-1 @3xl:block"></div>
    <div class="relative min-w-0 @3xl:w-44">
      <Search size={13} class="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-faint" />
      <input class="input input-sm pl-8" placeholder="Search" aria-label="Search SQL or connection" bind:value={q} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
    </div>
    <Select class="w-full @3xl:w-36" size="sm" bind:value={connId} options={connectionOptions} aria-label="Connection" />
  </div>

  <div class="min-h-0 flex-1 overflow-auto border-t border-line">
    {#if rows.length === 0}
      <EmptyState class="h-full justify-center" title="No matching entries" hint="Try another filter or search.">
        {#snippet icon()}<Search size={15} />{/snippet}
      </EmptyState>
    {:else}
      <table class="w-full min-w-[760px] table-fixed border-separate border-spacing-0 text-sm">
        <thead class="sticky top-0 z-sticky">
          <tr class="text-left [&>th]:border-b [&>th]:border-line [&>th]:bg-surface">
            <th scope="col" class="th w-[140px] py-2 pr-2 pl-9">Time</th>
            <th scope="col" class="th w-[128px] px-2 py-2">Connection</th>
            <th scope="col" class="th w-[52px] px-2 py-2">Type</th>
            <th scope="col" class="th w-[64px] px-2 py-2">By</th>
            <th scope="col" class="th px-2 py-2">SQL</th>
            <th scope="col" class="th w-[132px] px-2 py-2">Result</th>
            <th scope="col" class="th w-[52px] py-2 pr-4 pl-2 text-right">ms</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as e (e.id)}
            {@const c = conns[e.connId]}
            {@const open = openId === e.id}
            {@const time = fmtTime(e.time)}
            {@const detailId = `${pageId}-entry-${e.id}`}
            <tr class={open ? 'bg-hover' : 'hover:bg-hover focus-within:bg-hover'}>
              <td class="h-rich-menu-row border-b border-line/60 pr-2 pl-3 text-sm text-faint tabular-nums">
                <button
                  type="button"
                  class="focus-ring flex h-list-row w-full cursor-pointer items-center gap-2 rounded text-left hover:text-ink"
                  aria-label={`${open ? 'Hide' : 'Show'} query details for ${time}`}
                  aria-expanded={open}
                  aria-controls={open ? detailId : undefined}
                  onclick={() => (openId = open ? null : e.id)}>
                  <ChevronRight size={12} class="shrink-0 motion-safe:transition-transform motion-safe:duration-150 {open ? 'rotate-90' : ''}" />
                  <span class="truncate">{time}</span>
                </button>
              </td>
              <td class="truncate border-b border-line/60 px-2">
                <span class="mr-2 inline-block size-1.5 rounded-full align-middle {c ? envDot[c.env] : 'bg-faint'}" title={c && environmentLabel(c)}></span><span>{c?.name ?? e.connId}</span>{#if c}<span class="sr-only">, {environmentLabel(c)}</span>{/if}
              </td>
              <td class="border-b border-line/60 px-2">
                <span class="sentence text-sm font-medium {e.kind === 'WRITE' ? 'text-warn' : 'text-muted'}">{e.kind}</span>
              </td>
              <td class="truncate border-b border-line/60 px-2">
                <span class="text-sm {e.source === 'AI' ? 'font-medium text-accent' : 'text-muted'}">{e.source}</span>
              </td>
              <td class="truncate border-b border-line/60 px-2 font-mono text-sm text-muted">{e.sql.replace(/\s+/g, ' ')}</td>
              <td class="truncate border-b border-line/60 px-2">
                {#if e.result.status === 'ok'}
                  <span class="mr-1.5 inline-block size-1.5 rounded-full bg-ok align-middle"></span><span class="sr-only">Succeeded: </span><span class="tabular-nums">{resultText(e)}</span>
                {:else if e.result.status === 'error'}
                  <span class="mr-1.5 inline-block size-1.5 rounded-full bg-danger align-middle"></span><span class="text-danger" title={resultText(e)}>{resultText(e)}</span>
                {:else}
                  <span class="mr-1.5 inline-block size-1.5 rounded-full bg-warn align-middle"></span><span class="text-warn" title={resultText(e)}>{resultText(e)}</span>
                {/if}
              </td>
              <td class="border-b border-line/60 py-0 pr-4 pl-2 text-right font-mono text-xs text-faint tabular-nums">{isBlocked(e) ? '–' : e.ms}</td>
            </tr>
            {#if open}
              <tr id={detailId} class="bg-hover">
                <td colspan="7" class="border-b border-line/60 pt-0.5 pr-4 pb-3 pl-9">
                  <div class="relative rounded-lg border border-line bg-surface">
                    <pre class="overflow-auto px-3 py-2.5 pr-20 font-mono text-sm leading-relaxed whitespace-pre-wrap">{e.sql}</pre>
                    <button class="btn btn-sm btn-ghost absolute top-1.5 right-1.5" onclick={() => void copy(e)}>
                      {#if copied.current === e.id}<Check size={12} class="text-ok" /> Copied{:else}<Copy size={12} /> Copy{/if}
                    </button>
                  </div>
                </td>
              </tr>
            {/if}
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
  <footer class="flex h-sub-toolbar shrink-0 items-center gap-3 border-t border-line px-4 text-sm text-faint tabular-nums">
    <span>{rows.length} of {audit.entries.length} entries</span>
  </footer>
</div>
