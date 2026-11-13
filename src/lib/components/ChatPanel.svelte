<script lang="ts">
  import { ArrowLeft, FileCode, History, ListCollapse, Play, SquarePen, SquarePlus, Trash2, X } from '@lucide/svelte';
  import ChatTranscript from './ChatTranscript.svelte';
  import ChatComposer from './ChatComposer.svelte';
  import { chat, current, deleteSession, openSession, scopeOf, send, sessionsByDate } from '../chat.svelte';
  import { ago, sourceLabel } from '../chatParts';
  import { connOf, sameSource, type Conn, type Source } from '../connections.svelte';
  import { KEYS, isKey } from '../keys';
  import { Pref } from '../persist.svelte';
  import { ui } from '../state.svelte';
  import { activeTab } from '../tabs.svelte';
  import { classify } from '../writeCheck';
  import { focusFirst } from '../actions';

  let { tables = [], activeBusy = false, onEnableWrites, onInsertSql, onRunSql }: { tables?: string[]; activeBusy?: boolean; onEnableWrites: (c: Conn) => void; onInsertSql: (sql: string, source?: Source) => void; onRunSql: (sql: string, source?: Source, target?: 'active' | 'new') => void } = $props();

  const session = $derived(current());
  const messages = $derived(session?.messages ?? []);
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
  /** A runnable code block in the chat (SQL or mongosh), with the screen position of its Run button. */
  type RunMenu = { sql: string; source?: Source; kind: 'read' | 'write'; count: number; x: number; y: number; anchor: HTMLElement };
  let runMenu = $state<RunMenu | null>(null);
  const menuConn = $derived(runMenu?.source ? connOf(runMenu.source) : undefined);
  const newReady = $derived(!!runMenu?.source && !!menuConn);
  /** The code block's database differs from the active tab's, so "Run in active tab" opens a new tab there. */
  const activeSource = $derived<Source | undefined>(active ? { connId: active.connId, database: active.database } : undefined);
  const activeOther = $derived(!!activeSource && !!runMenu?.source && !!menuConn && !sameSource(activeSource, runMenu.source));
  const activeReady = $derived(activeOther || (!!active && !active.table && !activeBusy));
  const insertReady = $derived(!!active || newReady);

  /** Opens the small Run menu under the code block's button; picking an item runs at once. */
  async function openRunMenu(sql: string, anchor: HTMLElement, source?: Source) {
    if (runMenu?.anchor === anchor) {
      closeRunMenu();
      return;
    }
    const src = source ?? activeSource;
    const c = src ? connOf(src) : activeConn;
    // The same classification as a run: empty text has nothing to run, and text the guard cannot
    // parse counts as a write, so it can only be inserted for review.
    const cls = await classify(c?.engine ?? 'pg', sql, src?.database);
    if (cls.empty) return;
    const count = cls.count;
    const anyWrite = cls.write;
    const r = anchor.getBoundingClientRect();
    const width = 264;
    // A write menu has one warning line and one item instead of two run items.
    const height = anyWrite ? 176 : 132;
    const x = Math.max(8, Math.min(r.right - width, innerWidth - width - 8));
    const y = r.bottom + height + 8 > innerHeight ? Math.max(8, r.top - height - 8) : r.bottom + 8;
    runMenu = { sql, source: src, kind: anyWrite ? 'write' : 'read', count, x, y, anchor };
  }

  function closeRunMenu(refocus = true) {
    const a = runMenu?.anchor;
    runMenu = null;
    if (refocus) a?.focus();
  }

  function runFromMenu(target: 'active' | 'new') {
    const m = runMenu;
    if (!m) return;
    runMenu = null;
    if (m.kind === 'write') {
      onInsertSql(m.sql, m.source);
      return;
    }
    onRunSql(m.sql, m.source, target);
  }

  /** Writes are never executed from chat; insert them so the user can review and run them. */
  function insertFromMenu() {
    const m = runMenu;
    if (!m) return;
    runMenu = null;
    onInsertSql(m.sql, m.source);
  }

  function activeHint() {
    if (!active) return 'No query tab';
    if (activeOther) return 'A different database · opens a new tab';
    if (active.table) return 'The active tab shows a table';
    if (activeBusy) return 'The active tab is running a query';
    const where = sourceLabel({ connId: active.connId, database: active.database });
    const env = activeConn ? ` · ${activeConn.env}` : '';
    return active.query.trim() ? `${active.title} · ${where}${env} · appends below` : `${active.title} · ${where}${env} · empty`;
  }

  function newHint() {
    if (!runMenu?.source) return 'No database is available';
    if (!menuConn) return 'The connection is missing';
    return `${sourceLabel(runMenu.source)} · ${menuConn.env}`;
  }

  function insertHint() {
    if (!active) return 'No query tab';
    if (active.table) return 'Opens a new tab';
    if (activeSource && runMenu?.source && !sameSource(activeSource, runMenu.source)) return 'A different database · opens a new tab';
    return active.query.trim() ? `${active.title} · appends below` : `${active.title} · empty`;
  }

  $effect(() => {
    if (!runMenu) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        closeRunMenu();
      }
    };
    const onDown = (e: PointerEvent) => {
      const el = e.target as Element;
      if (!el.closest('.run-menu') && !el.closest('.code-run')) closeRunMenu(false);
    };
    const onScroll = () => closeRunMenu(false);
    window.addEventListener('keydown', onKey);
    window.addEventListener('pointerdown', onDown, true);
    window.addEventListener('scroll', onScroll, true);
    window.addEventListener('resize', onScroll);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('pointerdown', onDown, true);
      window.removeEventListener('scroll', onScroll, true);
      window.removeEventListener('resize', onScroll);
    };
  });

  let input = $state('');
  let showHistory = $state(false);
  let composer = $state<{ focusInput: () => void }>();

  // The scripted demo flows talk about orders; real connections get prompts for their own tables.
  // `write`: the prompt asks for a change, so it shows only while writes are on.
  const suggestions = $derived.by(() => {
    const out: { text: string; write?: boolean }[] = tables.includes('orders') ? [{ text: 'Which order status brings the most revenue?' }, { text: 'Delete cancelled orders older than 90 days', write: true }] : [];
    for (const t of tables.filter((t) => t !== 'orders').slice(0, 2)) out.push({ text: `Show the latest 20 rows of ${t}` });
    const mongoCount = scopeConns.filter((c) => c.engine === 'mongo').length;
    if (tables.length) {
      const noun = mongoCount === 0 ? 'tables' : mongoCount === scopeConns.length ? 'collections' : 'tables and collections';
      out.push({ text: `What ${noun} are in this database, and what do they hold?` });
    }
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

  function submit(text = input) {
    if (!text.trim()) return;
    if (chat.live || !ready) return;
    input = '';
    showHistory = false;
    send(text);
  }

  function retry(mi: number) {
    const q = messages[mi - 1];
    if (q?.role === 'user' && q.parts[0]?.kind === 'text') submit(q.parts[0].text);
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

<section class="card absolute inset-y-2 right-2 z-30 flex max-w-[calc(100%-1rem)] flex-col overflow-hidden shadow-2xl min-[880px]:relative min-[880px]:inset-auto min-[880px]:min-w-[280px] min-[880px]:shadow-[var(--card-shadow)]" style:width="{ui.chatWidth}px">
  <header class="relative flex h-11 shrink-0 items-center gap-1 border-b border-line pr-2 pl-4">
    <span class="mr-1 text-[13px] font-semibold">{showHistory ? 'Chats' : 'Assistant'}</span>
    <div class="flex-1"></div>
    {#if !showHistory}
      <button class="btn btn-ghost size-7 p-0 {showSteps ? '' : 'bg-hover text-ink'}" onclick={toggleSteps} aria-pressed={!showSteps} title={showSteps ? `Hide steps, show only results (${KEYS.toggleSteps.hint})` : `Show steps (${KEYS.toggleSteps.hint})`} aria-label={showSteps ? 'Hide steps' : 'Show steps'}><ListCollapse size={14} /></button>
    {/if}
    <button class="btn btn-ghost size-7 p-0 {showHistory ? 'bg-hover text-ink' : ''}" onclick={() => (showHistory = !showHistory)} disabled={!!chat.live} aria-pressed={showHistory} title={chat.live ? 'Wait for the reply to finish' : 'Chat history'} aria-label="Chat history"><History size={14} /></button>
    <button class="btn btn-ghost size-7 p-0" onclick={newChat} disabled={!!chat.live || (!session && !showHistory)} title="New chat" aria-label="New chat"><SquarePen size={14} /></button>
    <button class="btn btn-ghost size-7 p-0" onclick={() => (ui.chatOpen = false)} aria-label="Close assistant"><X size={14} /></button>
  </header>

  {#if showHistory}
    <div class="flex-1 overflow-auto px-2 pt-2 pb-3">
      <button class="nav-item mb-1 text-muted" onclick={() => (showHistory = false)}><ArrowLeft size={13} /> Back to chat</button>
      <div class="px-2 pt-2 pb-1.5 text-[11.5px] font-medium text-faint">All chats</div>
      {#each past as s (s.id)}
        <div class="group relative">
          <button class="flex w-full flex-col items-start gap-0.5 rounded-lg px-2.5 py-2 pr-9 text-left hover:bg-hover {s.id === session?.id ? 'bg-active' : ''}" onclick={() => pick(s.id)}>
            <span class="w-full truncate text-[12.5px] text-ink">{s.title}</span>
            <span class="w-full truncate text-[11.5px] text-faint" title={s.sources.map(sourceLabel).join(', ')}>{ago(s.updated)} · {Math.ceil(s.messages.length / 2)} {s.messages.length > 2 ? 'questions' : 'question'} · {s.sources.length === 1 ? sourceLabel(s.sources[0]) : `${s.sources.length} databases`}</span>
          </button>
          <button class="absolute top-1/2 right-1.5 grid size-6 -translate-y-1/2 place-items-center rounded text-faint opacity-0 group-hover:opacity-100 hover:bg-active hover:text-red-600 focus-visible:opacity-100" onclick={() => deleteSession(s.id)} title="Delete chat" aria-label="Delete chat {s.title}"><Trash2 size={13} /></button>
        </div>
      {:else}
        <p class="px-2.5 py-2 text-[12.5px] text-muted">No chats yet.</p>
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
    disabled={!!chat.live}
    {writesOn}
    {ready}
    onsubmit={() => submit()}
    onenablewrites={onEnableWrites}
    onkeydown={keyComposer}
  />
</section>

{#if runMenu}
  <div class="popover run-menu fixed z-[60] w-[264px] p-1" style:left="{runMenu.x}px" style:top="{runMenu.y}px" role="menu" aria-label="Run query" use:focusFirst>
    <div class="flex items-center gap-1.5 px-2 pt-1.5 pb-1 text-[10.5px]">
      <span class="font-mono font-semibold {runMenu.kind === 'write' ? 'text-amber-700 dark:text-amber-400' : 'text-emerald-700 dark:text-emerald-400'}">{runMenu.kind === 'write' ? 'WRITE' : 'READ'}</span>
      {#if runMenu.count > 1}<span class="shrink-0 text-faint">{runMenu.count} statements</span>{/if}
    </div>
    {#if runMenu.kind === 'write'}
      <p class="px-2 pb-1.5 text-[11px] leading-snug text-amber-700 dark:text-amber-400">Writes are not run from chat. Insert, review, then run them in the editor.</p>
      <button
        type="button"
        role="menuitem"
        class="flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover focus:bg-hover focus:outline-none disabled:cursor-default disabled:opacity-45 disabled:hover:bg-transparent"
        disabled={!insertReady}
        onclick={insertFromMenu}
      >
        <FileCode size={13} class="mt-0.5 shrink-0" />
        <span class="min-w-0 flex-1">
          <span class="block text-[12.5px] text-ink">Insert into editor</span>
          <span class="block truncate text-[11px] text-faint">{insertHint()}</span>
        </span>
      </button>
    {:else}
      <button
        type="button"
        role="menuitem"
        class="flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover focus:bg-hover focus:outline-none disabled:cursor-default disabled:opacity-45 disabled:hover:bg-transparent"
        disabled={!activeReady}
        onclick={() => runFromMenu('active')}
      >
        <Play size={13} class="mt-0.5 shrink-0" />
        <span class="min-w-0 flex-1">
          <span class="block text-[12.5px] text-ink">Run in active tab</span>
          <span class="block truncate text-[11px] text-faint">{activeHint()}</span>
        </span>
      </button>
      <button
        type="button"
        role="menuitem"
        class="flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover focus:bg-hover focus:outline-none disabled:cursor-default disabled:opacity-45 disabled:hover:bg-transparent"
        disabled={!newReady}
        onclick={() => runFromMenu('new')}
      >
        <SquarePlus size={13} class="mt-0.5 shrink-0" />
        <span class="min-w-0 flex-1">
          <span class="block text-[12.5px] text-ink">Run in a new tab</span>
          <span class="block truncate text-[11px] text-faint">{newHint()}</span>
        </span>
      </button>
    {/if}
  </div>
{/if}

<svelte:window onkeydown={(e) => { if (isKey(e, KEYS.toggleSteps.combo)) { e.preventDefault(); toggleSteps(); } }} />
