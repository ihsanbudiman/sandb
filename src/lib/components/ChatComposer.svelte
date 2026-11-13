<script lang="ts">
  import { ArrowUp, Info, ShieldCheck, ShieldOff, Square } from '@lucide/svelte';
  import ChatScope from './ChatScope.svelte';
  import ModelPicker from './ModelPicker.svelte';
  import { chat, setScope } from '../chat.svelte';
  import { engines, envTag, setAiWrite, type Conn, type Source } from '../connections.svelte';
  import { KEYS } from '../keys';

  let {
    value = $bindable(''),
    placeholder,
    scope,
    scopeConns,
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
</script>

<div class="mx-3 mb-3 rounded-xl border border-line bg-field p-2 transition-[border-color,box-shadow] focus-within:border-accent/70 focus-within:ring-3 focus-within:ring-accent/15">
  <ChatScope sources={scope} disabled={disabled} onchange={setScope} />
  <textarea
    rows="2"
    bind:this={composer}
    bind:value
    {placeholder}
    class="block max-h-42 w-full resize-none bg-transparent px-1 py-0.5 text-[13px] outline-none placeholder:text-faint"
    onkeydown={onkeydown}
    autocapitalize="off"
    spellcheck="false"
  ></textarea>
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
        {#each [['↵', 'Send'], ['⇧↵', 'New line'], ['↑', 'Last question (empty box)'], [KEYS.toggleSteps.hint, 'Show or hide steps']] as [k, t] (k)}
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
      <button class="btn btn-primary size-7 shrink-0 rounded-lg p-0" onclick={onsubmit} disabled={disabled || !value.trim() || !ready} aria-label="Send">
        <ArrowUp size={15} />
      </button>
    {/if}
  </div>
</div>
