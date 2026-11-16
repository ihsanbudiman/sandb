<script lang="ts">
  import { tick } from 'svelte';
  import { ArrowUp, BookOpen, Info, ShieldCheck, ShieldOff, Square, X } from '@lucide/svelte';
  import ChatScope from './ChatScope.svelte';
  import ModelPicker from './ModelPicker.svelte';
  import { scrollOn } from '../actions';
  import { SKILLS } from '../api/generated/constants';
  import { chat, setScope } from '../chat.svelte';
  import { engines, envTag, setAiWrite, type Conn, type Source } from '../connections.svelte';
  import { KEYS } from '../keys';
  import { appliesTo, skillTitle } from '../skills';

  let {
    value = $bindable(''),
    placeholder,
    scope,
    scopeConns,
    skills,
    onskills,
    disabled,
    writesOn,
    ready,
    onsubmit,
    onenablewrites,
    onkeydown,
  }: {
    value?: string;
    placeholder: string;
    scope: Source[];
    scopeConns: Conn[];
    /** Skills picked for this chat, by name. */
    skills: string[];
    onskills: (list: string[]) => void;
    disabled: boolean;
    writesOn: boolean;
    ready: boolean;
    onsubmit: () => void;
    onenablewrites: (c: Conn) => void;
    onkeydown: (e: KeyboardEvent) => void;
  } = $props();

  let composer = $state<HTMLTextAreaElement>();
  let writeMenu = $state(false);
  let keysMenu = $state(false);

  /** The `/` that the user is typing in the text box: where it starts, and the text after it that filters the menu. */
  let slash = $state<{ start: number; query: string } | null>(null);
  let hi = $state(0);

  const skillEngines = $derived(scopeConns.map((c) => c.engine));
  // Skills of the chat's engines that are not picked yet, narrowed by the text after the `/`.
  const matches = $derived.by(() => {
    if (!slash) return [];
    const q = slash.query.toLowerCase();
    return SKILLS.filter((s) => !skills.includes(s.name) && skillEngines.some((e) => s.engines.includes(e)) && (!q || s.name.toLowerCase().includes(q) || s.title.toLowerCase().includes(q)));
  });

  // Grows with its text up to about eight lines.
  $effect(() => {
    void value;
    if (!composer) return;
    composer.style.height = 'auto';
    composer.style.height = `${Math.min(composer.scrollHeight, 168)}px`;
  });

  /** Puts the caret back in the text box after a new chat was opened. */
  export function focusInput() {
    composer?.focus();
  }

  // On: writes off on every connection of the chat that has them on. Off: the safeguard dialog for the one connection, or a choice of them.
  function toggleMode() {
    if (writesOn) for (const c of scopeConns.filter((c) => c.aiWrite)) void setAiWrite(c, false);
    else if (scopeConns.length === 1) onenablewrites(scopeConns[0]);
    else writeMenu = !writeMenu;
  }

  // `/` at the start of the text or after a space opens the skill menu; the text up to the caret filters it.
  function onInput(e: Event) {
    if (disabled) {
      slash = null;
      return;
    }
    const el = e.currentTarget as HTMLTextAreaElement;
    const m = /(?:^|\s)\/(\S*)$/.exec(el.value.slice(0, el.selectionStart));
    if (!m) {
      slash = null;
      return;
    }
    const start = el.selectionStart - m[1].length - 1;
    if (slash?.start !== start || slash.query !== m[1]) hi = 0;
    slash = { start, query: m[1] };
  }

  // The open menu takes Enter, Tab and the arrows before the panel's key handling, so Enter picks a skill and never sends.
  function key(e: KeyboardEvent) {
    if (slash && menuKey(e)) return;
    if (e.key === 'Backspace' && !value && skills.length && !disabled) {
      e.preventDefault();
      onskills(skills.slice(0, -1));
      return;
    }
    onkeydown(e);
  }

  function menuKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      slash = null;
      return true;
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (matches.length) hi = (hi + (e.key === 'ArrowDown' ? 1 : matches.length - 1)) % matches.length;
      return true;
    }
    if ((e.key === 'Enter' || e.key === 'Tab') && !e.shiftKey) {
      // No match: the `/` is plain text (a path, a fraction), so the key does its usual job.
      if (!matches.length) {
        slash = null;
        return false;
      }
      e.preventDefault();
      void pick(matches[Math.min(hi, matches.length - 1)].name);
      return true;
    }
    return false;
  }

  /** Takes the `/token` out of the text and adds the skill as a chip. */
  async function pick(name: string) {
    const token = slash;
    slash = null;
    if (!skills.includes(name)) onskills([...skills, name]);
    if (!token || !composer) return;
    // The whole word after the `/`, not only the part that filtered the menu, so no fragment stays behind.
    let end = token.start + 1 + token.query.length;
    while (end < composer.value.length && !/\s/.test(composer.value[end])) end++;
    value = composer.value.slice(0, token.start) + composer.value.slice(end);
    await tick();
    composer.selectionStart = composer.selectionEnd = Math.min(token.start, composer.value.length);
    composer.focus();
  }
</script>

<div class="mx-3 mb-3 rounded-xl border border-line bg-field p-2 transition-[border-color,box-shadow] focus-within:border-accent/70 focus-within:ring-3 focus-within:ring-accent/15">
  <ChatScope sources={scope} disabled={disabled} onchange={setScope} />
  {#if skills.length}
    <div class="flex flex-wrap items-center gap-1 pb-1.5">
      {#each skills as name (name)}
        {@const applies = appliesTo(name, skillEngines)}
        <span class="flex h-6 max-w-full min-w-0 items-center gap-1 rounded-md border border-line bg-surface pr-0.5 pl-1.5 text-[11.5px] {applies ? '' : 'opacity-50'}" title={applies ? `Skill: ${skillTitle(name)}` : 'Does not apply to the databases in this chat'}>
          <BookOpen size={11} class="shrink-0 text-accent" />
          <span class="min-w-0 truncate text-ink">{skillTitle(name)}</span>
          <button class="grid size-4.5 shrink-0 place-items-center rounded text-faint hover:bg-hover hover:text-ink disabled:opacity-40" {disabled} onclick={() => onskills(skills.filter((n) => n !== name))} aria-label="Remove skill {skillTitle(name)}"><X size={11} /></button>
        </span>
      {/each}
    </div>
  {/if}
  <div class="relative">
    <textarea
      rows="2"
      bind:this={composer}
      bind:value
      {placeholder}
      class="block max-h-42 w-full resize-none bg-transparent px-1 py-0.5 text-[13px] outline-none placeholder:text-faint"
      oninput={onInput}
      onkeydown={key}
      autocapitalize="off"
      spellcheck="false"
    ></textarea>
    {#if slash}
      <div class="popover absolute inset-x-0 bottom-full z-50 mb-1 max-h-72 overflow-auto p-1" role="listbox" aria-label="Skills">
        {#each matches as s, i (s.name)}
          <button
            type="button"
            role="option"
            aria-selected={i === hi}
            class="flex w-full flex-col items-start gap-0.5 rounded-md px-2 py-1.5 text-left {i === hi ? 'bg-active text-ink' : ''}"
            use:scrollOn={i === hi}
            onmousemove={() => (hi = i)}
            onclick={() => void pick(s.name)}
          >
            <span class="flex w-full items-baseline gap-2">
              <span class="min-w-0 flex-1 truncate text-[12.5px] font-medium">{s.title}</span>
              <span class="shrink-0 font-mono text-[10.5px] font-normal text-faint">{s.name}</span>
            </span>
            <span class="line-clamp-2 text-[11.5px] leading-snug text-muted">{s.description}</span>
          </button>
        {:else}
          <p class="px-2 py-1.5 text-[12px] text-muted">No skills match</p>
        {/each}
        <p class="mt-1 border-t border-line px-2 pt-1.5 pb-1 text-[11px] text-faint">↑↓ choose · ↵ or Tab add · Esc close</p>
      </div>
      <!-- Under the open menu (z-50), over everything else. -->
      <button class="fixed inset-0 z-40 cursor-default" aria-label="Close" onclick={() => (slash = null)}></button>
    {/if}
  </div>
  <div class="relative mt-1 flex items-center gap-2">
    {#if writesOn}
      <button class="flex h-6 shrink-0 items-center gap-1.5 rounded-md px-1.5 text-[12px] font-medium whitespace-nowrap text-amber-700 hover:bg-hover dark:text-amber-400" onclick={toggleMode} title="Writes need approval on {scopeConns.filter((c) => c.aiWrite).map((c) => c.name).join(', ')}. Click to switch writes off.">
        <ShieldOff size={13} /> Writes need approval
      </button>
    {:else}
      <button class="flex h-6 shrink-0 items-center gap-1.5 rounded-md px-1.5 text-[12px] font-medium whitespace-nowrap text-muted hover:bg-hover hover:text-ink disabled:opacity-40" onclick={toggleMode} disabled={!ready} aria-expanded={scopeConns.length > 1 ? writeMenu : undefined} title="Allow the assistant to propose writes">
        <ShieldCheck size={13} class="text-emerald-600 dark:text-emerald-400" /> Writes off
      </button>
    {/if}
    {#if writeMenu && !writesOn}
      <div class="popover absolute bottom-full left-0 z-50 mb-1 w-60 p-1">
        <div class="label px-2 pt-1.5 pb-1">Allow writes on</div>
        {#each scopeConns as c (c.id)}
          <button class="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[12.5px] hover:bg-hover" onclick={() => { writeMenu = false; onenablewrites(c); }}>
            <span class="w-5 shrink-0 font-mono text-[10.5px] font-semibold {engines[c.engine].badge}">{engines[c.engine].short}</span>
            <span class="min-w-0 flex-1 truncate text-ink">{c.name}</span>
            <span class="tag {envTag[c.env]}">{c.env}</span>
          </button>
        {/each}
      </div>
      <button class="fixed inset-0 z-40 cursor-default" aria-label="Close" onclick={() => (writeMenu = false)}></button>
    {/if}
    <ModelPicker variant="chip" />
    <button class="btn btn-ghost ml-auto size-6 shrink-0 p-0 {keysMenu ? 'bg-hover text-ink' : ''}" onclick={() => (keysMenu = !keysMenu)} aria-expanded={keysMenu} title="Keyboard shortcuts" aria-label="Keyboard shortcuts"><Info size={13} /></button>
    {#if keysMenu}
      <div class="popover absolute right-0 bottom-full z-50 mb-1 w-56 p-2 text-[12px]">
        <div class="label pb-1.5">Keyboard shortcuts</div>
        {#each [['↵', 'Send'], ['⇧↵', 'New line'], ['↑', 'Last question (empty box)'], ['/', 'Skills'], [KEYS.toggleSteps.hint, 'Show or hide steps']] as [k, t] (k)}
          <div class="flex h-6 items-center gap-2">
            <span class="min-w-0 flex-1 truncate text-muted">{t}</span>
            <kbd class="shrink-0 rounded border border-line px-1 font-mono text-[11px] text-ink">{k}</kbd>
          </div>
        {/each}
      </div>
      <button class="fixed inset-0 z-40 cursor-default" aria-label="Close" onclick={() => (keysMenu = false)}></button>
    {/if}
    {#if chat.live && chat.cancel}
      <button class="btn btn-primary size-7 shrink-0 rounded-lg p-0" onclick={() => chat.cancel?.()} aria-label="Stop" title="Stop the reply">
        <Square size={11} class="fill-current" />
      </button>
    {:else}
      <button class="btn btn-primary size-7 shrink-0 rounded-lg p-0" onclick={() => { slash = null; onsubmit(); }} disabled={disabled || !value.trim() || !ready} aria-label="Send">
        <ArrowUp size={15} />
      </button>
    {/if}
  </div>
</div>
