<script lang="ts" module>
  /** A table the palette can open. `key` is unique across connections and databases; `hint` names where it lives. */
  export type PaletteTable = { key: string; label: string; hint?: string; open: () => void };
</script>

<script lang="ts">
  import { Search, FileCode, Play, PanelRight, PanelLeft, Settings, ScrollText, SunMoon, Plus, Table2, Database, Save } from '@lucide/svelte';
  import { scrollOn } from '../actions';
  import Modal from './Modal.svelte';
  import { db, envDot, type Conn } from '../connections.svelte';
  import { KEYS } from '../keys';
  import { active } from '../tabs.svelte';
  import { saved, type SavedQuery } from '../saved.svelte';

  type Item = { id: string; group: string; label: string; icon?: typeof Search; dot?: string; env?: string; hint?: string; kbd?: boolean; run: () => void };

  let {
    open = $bindable(false),
    tables,
    onfocuseditor,
    onrun,
    ontogglechat,
    ontogglesidebar,
    onpage,
    ontoggletheme,
    onnewconnection,
    onswitch,
    onsave,
    onopensaved,
  }: {
    open?: boolean;
    tables: PaletteTable[];
    onfocuseditor: () => void;
    onrun: () => void;
    ontogglechat: () => void;
    ontogglesidebar: () => void;
    onpage: (p: 'settings' | 'audit') => void;
    ontoggletheme: () => void;
    onnewconnection: () => void;
    onswitch: (c: Conn) => void;
    onsave: () => void;
    onopensaved: (q: SavedQuery) => void;
  } = $props();

  let q = $state('');
  let sel = $state(0);

  const items = $derived<Item[]>([
    { id: 'a-new', group: 'Actions', label: 'New query / focus editor', icon: FileCode, run: onfocuseditor },
    { id: 'a-run', group: 'Actions', label: 'Run statement at cursor', icon: Play, hint: KEYS.run.hint, kbd: true, run: onrun },
    { id: 'a-save', group: 'Actions', label: 'Save query', icon: Save, hint: KEYS.save.hint, kbd: true, run: onsave },
    { id: 'a-chat', group: 'Actions', label: 'Toggle AI chat', icon: PanelRight, run: ontogglechat },
    { id: 'a-sidebar', group: 'Actions', label: 'Toggle sidebar', icon: PanelLeft, run: ontogglesidebar },
    { id: 'a-settings', group: 'Actions', label: 'Open Settings', icon: Settings, run: () => onpage('settings') },
    { id: 'a-audit', group: 'Actions', label: 'Open Audit log', icon: ScrollText, run: () => onpage('audit') },
    { id: 'a-theme', group: 'Actions', label: 'Toggle theme', icon: SunMoon, run: ontoggletheme },
    { id: 'a-conn', group: 'Actions', label: 'New connection', icon: Plus, run: onnewconnection },
    ...saved.list.map((s) => ({ id: 's-' + s.id, group: 'Saved queries', label: s.name, icon: FileCode, hint: db.connections.find((c) => c.id === s.connId)?.name, run: () => onopensaved(s) })),
    ...tables.map((t) => ({ id: 't-' + t.key, group: 'Tables', label: t.label, icon: Table2, hint: t.hint, run: t.open })),
    ...db.connections.map((c) => ({
      id: 'c-' + c.id,
      group: 'Connections',
      label: c.name,
      icon: Database,
      dot: envDot[c.env],
      env: c.env,
      hint: c.id === active().id ? 'active' : c.env,
      run: () => onswitch(c),
    })),
  ]);

  // Subsequence match: "opau" finds "Open Audit log".
  function matches(label: string, query: string) {
    const s = label.toLowerCase();
    let i = 0;
    for (const ch of query.toLowerCase().replace(/\s+/g, '')) {
      i = s.indexOf(ch, i) + 1;
      if (i === 0) return false;
    }
    return true;
  }

  // A database with thousands of tables (e.g. daily partitions) would otherwise render one row each.
  const MAX_TABLES = 50;
  const filtered = $derived.by(() => {
    let n = 0;
    return items.filter((it) => matches(it.label, q) && (it.group !== 'Tables' || ++n <= MAX_TABLES));
  });

  $effect(() => {
    if (open) {
      q = '';
      sel = 0;
    }
  });

  const focus = (el: HTMLInputElement) => el.focus();

  function close() {
    open = false;
  }

  function exec(it: Item | undefined) {
    if (!it) return;
    close();
    it.run();
  }

  function move(d: number) {
    const n = filtered.length;
    if (!n) return;
    sel = (sel + d + n) % n;
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') { e.preventDefault(); move(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
    else if (e.key === 'Enter') { e.preventDefault(); exec(filtered[sel]); }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
  }
</script>

{#if open}
  <Modal bare layout="top" width="max-w-xl" class="flex max-h-[60vh] flex-col overflow-hidden" label="Command palette" dismissOnBackdrop onclose={close}>
      <div class="flex items-center gap-2.5 border-b border-line px-4">
        <Search size={15} class="shrink-0 text-faint" />
        <input
          use:focus
          bind:value={q}
          oninput={() => (sel = 0)}
          onkeydown={onkey}
          class="h-12 flex-1 bg-transparent text-[14px] outline-none placeholder:text-faint"
          placeholder="Search actions, saved queries, tables…"
          autocapitalize="off"
          autocorrect="off"
          spellcheck="false"
          autocomplete="off"
        />
        <kbd class="kbd">esc</kbd>
      </div>
      <div class="flex-1 overflow-auto p-1.5">
        {#each filtered as it, i (it.id)}
          {#if i === 0 || filtered[i - 1].group !== it.group}
            <div class="label px-2.5 pt-2 pb-1 text-faint">{it.group}</div>
          {/if}
          <button
            data-i={i}
            class="flex h-8 w-full items-center gap-2.5 rounded-md px-2.5 text-left text-[13px] {i === sel ? 'bg-active text-ink' : 'text-muted'}"
            use:scrollOn={i === sel}
            onmousemove={() => (sel = i)}
            onclick={() => exec(it)}
          >
            {#if it.icon}<it.icon size={14} class={i === sel ? 'text-ink' : 'text-faint'} />{/if}
            <span class="flex-1 truncate {it.group === 'Tables' ? 'font-mono text-[12.5px]' : ''}">{it.label}</span>
            {#if it.dot}<span class="size-1.5 shrink-0 rounded-full {it.dot}" title={it.env}></span>{#if it.env !== it.hint}<span class="sr-only">{it.env}</span>{/if}{/if}
            {#if it.hint}
              {#if it.kbd}
                <kbd class="kbd {i === sel ? 'text-muted' : ''}">{it.hint}</kbd>
              {:else}
                <span class="text-[11.5px] {i === sel ? 'text-muted' : 'text-faint'}">{it.hint}</span>
              {/if}
            {/if}
          </button>
        {:else}
          <div class="px-2 py-8 text-center text-[13px] text-faint">No results</div>
        {/each}
      </div>
      <div class="flex items-center gap-4 border-t border-line px-4 py-2 text-[11.5px] text-faint">
        <span class="flex items-center gap-1.5"><kbd class="kbd">↑↓</kbd> navigate</span>
        <span class="flex items-center gap-1.5"><kbd class="kbd">↵</kbd> open</span>
        <span class="flex items-center gap-1.5"><kbd class="kbd">esc</kbd> close</span>
      </div>
  </Modal>
{/if}
