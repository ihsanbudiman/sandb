<script lang="ts">
  import { ArrowLeft, Search, Copy, Check, ChevronRight } from '@lucide/svelte';
  import { db, envDot } from '../connections.svelte';
  import { ui } from '../state.svelte';
  import { audit, type AuditEntry } from '../audit.svelte';
  import { Flash } from '../flash.svelte';

  type Tab = 'all' | 'read' | 'write' | 'blocked';

  let tab = $state<Tab>('all');
  let q = $state('');
  let connId = $state('all');
  let openId = $state<number | null>(null);
  const copied = new Flash<number>();

  const conns = $derived(Object.fromEntries(db.connections.map((c) => [c.id, c])));
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

<div class="flex h-full flex-col">
  <header class="flex h-12 shrink-0 items-center gap-2.5 px-4">
    <button class="btn btn-ghost -ml-1.5 size-7 p-0" onclick={() => (ui.page = 'query')} aria-label="Back to query"><ArrowLeft size={15} /></button>
    <h1 class="text-[14px] font-semibold">Audit log</h1>
    <span class="truncate text-[12.5px] text-faint">Every query run by you or the AI</span>
  </header>

  <div class="flex shrink-0 flex-wrap items-center gap-2 px-4 pb-3">
    <div class="seg">
      {#each tabs as t (t.id)}
        <button
          class="seg-item flex items-center gap-1.5 {tab === t.id ? 'seg-on' : ''}"
          onclick={() => (tab = t.id)}>{t.label} <span class="text-[11.5px] font-normal text-faint tabular-nums">{t.n}</span></button>
      {/each}
    </div>
    <div class="flex-1"></div>
    <div class="relative w-44">
      <Search size={13} class="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-faint" />
      <input class="input h-7 pl-8 text-[12.5px]" placeholder="Search SQL or connection" bind:value={q} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
    </div>
    <select class="input h-7 w-36 pr-1 text-[12.5px]" bind:value={connId} aria-label="Connection">
      <option value="all">All connections</option>
      {#each db.connections as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
    </select>
  </div>

  <div class="min-h-0 flex-1 overflow-auto border-t border-line">
    {#if rows.length === 0}
      <div class="grid h-full place-items-center">
        <div class="text-center">
          <div class="font-medium text-ink">No matching entries</div>
          <div class="mt-1 text-[12.5px] text-muted">Try another filter or search.</div>
        </div>
      </div>
    {:else}
      <table class="w-full table-fixed border-separate border-spacing-0 text-[12.5px]">
        <thead class="sticky top-0 z-10">
          <tr class="text-left">
            <th class="label w-[140px] border-b border-line bg-surface py-2 pr-2 pl-9 font-medium">Time</th>
            <th class="label w-[128px] border-b border-line bg-surface px-2 py-2 font-medium">Connection</th>
            <th class="label w-[52px] border-b border-line bg-surface px-2 py-2 font-medium">Type</th>
            <th class="label w-[40px] border-b border-line bg-surface px-2 py-2 font-medium">By</th>
            <th class="label border-b border-line bg-surface px-2 py-2 font-medium">SQL</th>
            <th class="label w-[132px] border-b border-line bg-surface px-2 py-2 font-medium">Result</th>
            <th class="label w-[52px] border-b border-line bg-surface py-2 pr-4 pl-2 text-right font-medium">ms</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as e (e.id)}
            {@const c = conns[e.connId]}
            {@const open = openId === e.id}
            <tr class="cursor-pointer {open ? 'bg-hover' : 'hover:bg-hover'}" onclick={() => (openId = open ? null : e.id)}>
              <td class="h-8 truncate border-b border-line/60 pr-2 pl-3 text-[12px] text-faint tabular-nums">
                <ChevronRight size={12} class="mr-2 inline text-faint transition-transform {open ? 'rotate-90' : ''}" />{fmtTime(e.time)}
              </td>
              <td class="truncate border-b border-line/60 px-2">
                <span class="mr-2 inline-block size-1.5 rounded-full align-middle {c ? envDot[c.env] : 'bg-stone-500 dark:bg-stone-400'}" title={c?.env}></span><span>{c?.name ?? e.connId}</span>{#if c}<span class="sr-only">, {c.env}</span>{/if}
              </td>
              <td class="border-b border-line/60 px-2">
                <span class="sentence text-[12px] font-medium {e.kind === 'WRITE' ? 'text-amber-700 dark:text-amber-400' : 'text-muted'}">{e.kind}</span>
              </td>
              <td class="border-b border-line/60 px-2">
                <span class="text-[12px] {e.source === 'AI' ? 'font-medium text-accent' : 'text-muted'}">{e.source}</span>
              </td>
              <td class="truncate border-b border-line/60 px-2 font-mono text-[12px] text-muted">{e.sql.replace(/\s+/g, ' ')}</td>
              <td class="truncate border-b border-line/60 px-2">
                {#if e.result.status === 'ok'}
                  <span class="mr-1.5 inline-block size-1.5 rounded-full bg-emerald-500 align-middle"></span><span class="sr-only">Succeeded: </span><span class="tabular-nums">{resultText(e)}</span>
                {:else if e.result.status === 'error'}
                  <span class="mr-1.5 inline-block size-1.5 rounded-full bg-red-500 align-middle"></span><span class="sr-only">Error: </span><span class="text-red-700 dark:text-red-400" title={resultText(e)}>{resultText(e)}</span>
                {:else}
                  <span class="mr-1.5 inline-block size-1.5 rounded-full bg-amber-500 align-middle"></span><span class="sr-only">{e.result.status === 'blocked' ? 'blocked' : 'denied'} </span><span class="text-amber-700 dark:text-amber-400" title={resultText(e)}>{resultText(e)}</span>
                {/if}
              </td>
              <td class="border-b border-line/60 py-0 pr-4 pl-2 text-right font-mono text-[11.5px] text-faint tabular-nums">{e.result.status === 'blocked' || e.result.status === 'denied' ? '–' : e.ms}</td>
            </tr>
            {#if open}
              <tr class="bg-hover">
                <td colspan="7" class="border-b border-line/60 pt-0.5 pr-4 pb-3 pl-9">
                  <div class="relative rounded-lg border border-line bg-surface">
                    <pre class="overflow-auto px-3 py-2.5 pr-20 font-mono text-[12px] leading-[1.6] whitespace-pre-wrap">{e.sql}</pre>
                    <button class="btn btn-ghost absolute top-1.5 right-1.5 h-6 px-2 text-[12px]" onclick={(ev) => { ev.stopPropagation(); copy(e); }}>
                      {#if copied.current === e.id}<Check size={12} class="text-emerald-600 dark:text-emerald-400" /> Copied{:else}<Copy size={12} /> Copy{/if}
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
  <footer class="flex h-9 shrink-0 items-center gap-3 border-t border-line px-4 text-[12px] text-faint tabular-nums">
    <span>{rows.length} of {audit.entries.length} entries</span>
  </footer>
</div>
