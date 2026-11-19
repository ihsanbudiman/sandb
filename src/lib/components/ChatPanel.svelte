<script lang="ts">
  import { ArrowLeft, CalendarClock, FileCode, History, ListCollapse, LoaderCircle, Play, SquarePen, SquarePlus, Trash2, X } from '@lucide/svelte';
  import ChatTranscript from './ChatTranscript.svelte';
  import ChatComposer from './ChatComposer.svelte';
  import Menu from './ui/Menu.svelte';
  import { chat, current, deleteSession, openSession, scopeOf, send, sessionsByDate, setSkills, skillsOf } from '../chat.svelte';
  import { ago, sourceLabel } from '../chatParts';
  import { environmentLabel, connOf, sameSource, type Conn, type Source } from '../connections.svelte';
  import { KEYS, isKey } from '../keys';
  import { Pref } from '../persist.svelte';
  import { ui } from '../state.svelte';
  import { activeTab } from '../tabs.svelte';
  import { classify, type Classification } from '../writeCheck';

  let { tables = [], activeBusy = false, onEnableWrites, onInsertSql, onRunSql }: { tables?: string[]; activeBusy?: boolean; onEnableWrites: (c: Conn) => void; onInsertSql: (sql: string, source?: Source) => void; onRunSql: (sql: string, source?: Source, target?: 'active' | 'new') => void } = $props();

  const session = $derived(current());
  const messages = $derived(session?.messages ?? []);
  const skills = $derived(skillsOf(session));
  const past = $derived(sessionsByDate());
  const scope = $derived(scopeOf(session));
  /** The connections of the chat's databases, each once; deleted ones are left out. */
  const scopeConns = $derived([...new Set(scope.map((s) => connOf(s)).filter((c): c is Conn => !!c))]);
  const writesOn = $derived(scopeConns.some((c) => c.aiWrite));
  const many = $derived(scope.length > 1);
  /** At least one database of the chat still has its connection. */
  const ready = $derived(scopeConns.length > 0);

  const active = $derived(activeTab());
  const activeConn = $derived(active ? connOf({ connId: active.connId, database: active.database }) : undefined);
  type RunMenu = { sql: string; source?: Source; anchor: HTMLElement };
  let runMenu = $state.raw<RunMenu | null>(null);
  let runResult = $state.raw<Classification | null>(null);
  const menuConn = $derived(runMenu?.source ? connOf(runMenu.source) : undefined);
  const newReady = $derived(!!runMenu?.source && !!menuConn);
  /** The code block's database differs from the active tab's, so "Run in active tab" opens a new tab there. */
  const activeSource = $derived<Source | undefined>(active ? { connId: active.connId, database: active.database } : undefined);
  const activeOther = $derived(!!activeSource && !!runMenu?.source && !!menuConn && !sameSource(activeSource, runMenu.source));
  const activeReady = $derived(activeOther || (!!active && !active.table && !activeBusy));
  const insertReady = $derived(!!active || newReady);

  /** Open before asking the guard; only the current request may reveal actions. */
  async function openRunMenu(sql: string, anchor: HTMLElement, source?: Source) {
    if (runMenu?.anchor === anchor) {
      runMenu = null;
      return;
    }
    const src = source ?? activeSource;
    const c = src ? connOf(src) : activeConn;
    const request = { sql, source: src, anchor };
    runResult = null;
    runMenu = request;
    const result = await classify(c?.engine ?? 'pg', sql, src?.database);
    if (runMenu !== request) return;
    if (result.empty || !anchor.isConnected) runMenu = null;
    else runResult = result;
  }

  function activeHint() {
    if (!active) return 'No query tab';
    if (activeOther) return 'A different database · opens a new tab';
    if (active.table) return 'The active tab shows a table';
    if (activeBusy) return 'The active tab is running a query';
    const where = sourceLabel({ connId: active.connId, database: active.database });
    const env = activeConn ? ` · ${environmentLabel(activeConn)}` : '';
    return active.query.trim() ? `${active.title} · ${where}${env} · appends below` : `${active.title} · ${where}${env} · empty`;
  }

  function newHint() {
    if (!runMenu?.source) return 'No database is available';
    if (!menuConn) return 'The connection is missing';
    return `${sourceLabel(runMenu.source)} · ${environmentLabel(menuConn)}`;
  }

  function insertHint() {
    if (!active) return 'No query tab';
    if (active.table) return 'Opens a new tab';
    if (activeSource && runMenu?.source && !sameSource(activeSource, runMenu.source)) return 'A different database · opens a new tab';
    return active.query.trim() ? `${active.title} · appends below` : `${active.title} · empty`;
  }

  $effect(() => {
    if (!ui.chatOpen) runMenu = null;
  });

  const runAnchor = $derived(runMenu?.anchor);
  $effect(() => {
    if (!runAnchor) return;
    const anchor = runAnchor;
    anchor.setAttribute('aria-expanded', 'true');
    return () => anchor.setAttribute('aria-expanded', 'false');
  });

  let input = $state('');
  let showHistory = $state(false);
  let composer = $state<{ focusInput: () => void }>();

  // The scripted demo flows talk about orders; real connections get prompts for their own tables.
  // `write`: the prompt asks for a change, so it shows only while writes are on.
  const suggestions = $derived.by(() => {
    const redisCount = scopeConns.filter((c) => c.engine === 'redis').length;
    if (redisCount && (redisCount === scopeConns.length || activeConn?.engine === 'redis')) return [
      { text: 'Which Redis keys are in this database?' },
      { text: 'How many Redis keys are in this database?' },
    ];
    const out: { text: string; write?: boolean }[] = tables.includes('orders') ? [{ text: 'Which order status brings the most revenue?' }, { text: 'Delete cancelled orders older than 90 days', write: true }] : [];
    for (const t of tables.filter((t) => t !== 'orders').slice(0, 2)) out.push({ text: `Show the latest 20 rows of ${t}` });
    const mongoCount = scopeConns.filter((c) => c.engine === 'mongo').length;
    if (tables.length) {
      const noun = mongoCount === 0 ? 'tables' : mongoCount === scopeConns.length ? 'collections' : 'tables and collections';
      out.push({ text: `What ${noun} are in this database, and what do they hold?` });
    }
    if (redisCount) out.unshift({ text: 'Which Redis keys are in this database?' });
    return out.filter((s) => writesOn || !s.write).slice(0, 4);
  });

  const stepsPref = new Pref('sandb.chat.steps', true, { version: 1, migrate: (v) => v !== 'hidden' && v !== false });
  const showSteps = $derived(stepsPref.value);
  function toggleSteps() {
    stepsPref.set(!stepsPref.value);
  }

  function keyComposer(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submit();
    } else if (e.key === 'ArrowUp' && !input && !chat.live) {
      // Up in an empty composer brings back the last question, to change and send again.
      const last = [...messages].reverse().find((m) => m.role === 'user')?.parts[0];
      if (last?.kind !== 'text') return;
      e.preventDefault();
      input = last.text;
    }
  }

  function submit(text = input, chosen?: string[]) {
    if (!text.trim()) return;
    if (chat.live || !ready) return;
    input = '';
    showHistory = false;
    send(text, chosen);
  }

  function retry(mi: number) {
    const q = messages[mi - 1];
    // The retry carries the skills of that question, not the chips of the composer now.
    if (q?.role === 'user' && q.parts[0]?.kind === 'text') submit(q.parts[0].text, q.skills ?? []);
  }

  function newChat() {
    openSession('');
    showHistory = false;
    composer?.focusInput();
  }

  function pick(id: string) {
    openSession(id);
    showHistory = false;
  }

  const placeholder = $derived(chat.pending ? 'Approve or deny the write above…' : ready ? (many ? 'Ask across these databases…' : 'Ask about your data…') : 'Add a database to ask about…');
</script>

<section class="card flex h-full min-h-0 w-full flex-col overflow-hidden">
  <header class="flex h-pane-header shrink-0 items-center gap-1 border-b border-line pr-2 pl-4">
    <span class="mr-1 text-lg font-semibold">{showHistory ? 'Chats' : 'Assistant'}</span>
    <div class="flex-1"></div>
    {#if !showHistory}
      <button class="btn btn-ghost btn-on size-7 p-0" onclick={toggleSteps} aria-pressed={!showSteps} title={showSteps ? `Hide steps, show only results (${KEYS.toggleSteps.hint})` : `Show steps (${KEYS.toggleSteps.hint})`} aria-label={showSteps ? 'Hide steps' : 'Show steps'}><ListCollapse size={15} /></button>
    {/if}
    <button class="btn btn-ghost btn-on size-7 p-0" onclick={() => (showHistory = !showHistory)} disabled={!!chat.live} aria-pressed={showHistory} title={chat.live ? 'Wait for the reply to finish' : 'Chat history'} aria-label="Chat history"><History size={15} /></button>
    <button class="btn btn-ghost size-7 p-0" onclick={newChat} disabled={!!chat.live || (!session && !showHistory)} title="New chat" aria-label="New chat"><SquarePen size={15} /></button>
    <button class="btn btn-ghost size-7 p-0" onclick={() => (ui.chatOpen = false)} aria-label="Close assistant"><X size={15} /></button>
  </header>

  {#if showHistory}
    <div class="flex-1 overflow-auto px-2 pt-2 pb-3">
      <button class="nav-item mb-1 text-muted" onclick={() => (showHistory = false)}><ArrowLeft size={13} /> Back to chat</button>
      <div class="label px-2 pt-2 pb-1.5 text-faint">All chats</div>
      {#each past as s (s.id)}
        <div class="group relative">
          <button class="focus-ring flex w-full flex-col items-start gap-0.5 rounded-lg px-2.5 py-2 pr-9 text-left hover:bg-hover {s.id === session?.id ? 'bg-active' : ''}" onclick={() => pick(s.id)}>
            <span class="w-full truncate text-sm text-ink">{#if s.scheduleId}<CalendarClock size={12} class="mr-1 inline text-accent" />{/if}{s.title}</span>
            <span class="w-full truncate text-xs text-faint" title={s.sources.map(sourceLabel).join(', ')}>{ago(s.updated)} · {Math.ceil(s.messages.length / 2)} {s.messages.length > 2 ? 'questions' : 'question'} · {s.sources.length === 1 ? sourceLabel(s.sources[0]) : `${s.sources.length} databases`}</span>
          </button>
          <button class="btn btn-sm btn-ghost nav-item-danger absolute top-1/2 right-1.5 size-6 -translate-y-1/2 p-0 opacity-0 group-hover:opacity-100 focus-visible:opacity-100" onclick={() => deleteSession(s.id)} title="Delete chat" aria-label="Delete chat {s.title}"><Trash2 size={13} /></button>
        </div>
      {:else}
        <p class="px-2.5 py-2 text-sm text-muted">No chats yet.</p>
      {/each}
    </div>
  {:else}
    <ChatTranscript
      {session}
      {messages}
      {scope}
      {scopeConns}
      {many}
      {showSteps}
      {activeSource}
      {suggestions}
      oninsert={onInsertSql}
      onrun={(sql, source) => onRunSql(sql, source)}
      onrunmenu={openRunMenu}
      onsubmit={(text) => submit(text)}
      onretry={retry}
    />
  {/if}

  <ChatComposer
    bind:this={composer}
    bind:value={input}
    {placeholder}
    {scope}
    {scopeConns}
    {skills}
    onskills={setSkills}
    disabled={!!chat.live}
    {writesOn}
    {ready}
    onsubmit={() => submit()}
    onenablewrites={onEnableWrites}
    onkeydown={keyComposer}
  />
</section>

{#each runMenu ? [runMenu] : [] as { sql, source, anchor } (anchor)}
  <Menu {anchor} align="end" label="Run query" class="w-64" onclose={() => (runMenu = null)}>
    {#if !runResult}
      <p class="flex h-list-row items-center gap-2 px-2 text-sm text-muted" role="status">
        <LoaderCircle size={13} class="motion-safe:animate-spin" /> Checking query…
      </p>
    {:else if menuConn?.engine === 'redis' && runResult.unparsable}
      <p class="px-2 py-1.5 text-sm text-danger" role="alert">{runResult.info?.warnings[0]?.message ?? 'Cannot check this command'}</p>
    {:else}
      {@const write = runResult.write}
      <div class="flex items-center gap-1.5 px-2 pt-1.5 pb-1 text-2xs">
        <span class="font-mono font-semibold {write ? 'text-warn' : 'text-ok'}">{write ? 'WRITE' : 'READ'}</span>
        {#if runResult.count > 1}<span class="shrink-0 text-faint">{runResult.count} statements</span>{/if}
      </div>
      {#if write}
        <p class="px-2 pb-1.5 text-xs leading-snug text-warn">Writes are not run from chat. Insert, review, then run them in the editor.</p>
        <button type="button" role="menuitem" tabindex="-1" class="menu-item h-rich-menu-row" disabled={!insertReady} onclick={() => onInsertSql(sql, source)}>
          <FileCode size={13} class="shrink-0" />
          <span class="min-w-0 flex-1 leading-tight">
            <span class="block text-ink">Insert into editor</span>
            <span class="block truncate text-xs text-faint">{insertHint()}</span>
          </span>
        </button>
      {:else}
        <button type="button" role="menuitem" tabindex="-1" class="menu-item h-rich-menu-row" disabled={!activeReady} onclick={() => onRunSql(sql, source, 'active')}>
          <Play size={13} class="shrink-0" />
          <span class="min-w-0 flex-1 leading-tight">
            <span class="block text-ink">Run in active tab</span>
            <span class="block truncate text-xs text-faint">{activeHint()}</span>
          </span>
        </button>
        <button type="button" role="menuitem" tabindex="-1" class="menu-item h-rich-menu-row" disabled={!newReady} onclick={() => onRunSql(sql, source, 'new')}>
          <SquarePlus size={13} class="shrink-0" />
          <span class="min-w-0 flex-1 leading-tight">
            <span class="block text-ink">Run in a new tab</span>
            <span class="block truncate text-xs text-faint">{newHint()}</span>
          </span>
        </button>
      {/if}
    {/if}
  </Menu>
{/each}

<svelte:window onkeydown={(e) => { if (isKey(e, KEYS.toggleSteps.combo)) { e.preventDefault(); toggleSteps(); } }} />
