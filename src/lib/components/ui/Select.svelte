<script lang="ts" generics="T extends string | number">
  import { Check, ChevronsUpDown } from '@lucide/svelte';
  import type { HTMLButtonAttributes } from 'svelte/elements';
  import { placePopup } from './popup';

  let {
    value = $bindable(),
    trigger = $bindable(),
    options,
    placeholder = 'Choose…',
    size = 'md',
    disabled = false,
    class: cls = '',
    'aria-label': label,
    'aria-labelledby': labelledBy,
    onchange,
    ...button
  }: Omit<HTMLButtonAttributes, 'children' | 'size' | 'onclick' | 'onkeydown' | 'onchange' | 'type' | 'role' | 'aria-expanded' | 'aria-haspopup' | 'aria-controls'> & {
    value: T;
    trigger?: HTMLButtonElement;
    options: readonly { value: T; label: string; disabled?: boolean }[];
    placeholder?: string;
    size?: 'md' | 'sm';
    onchange?: (value: T) => void;
  } = $props();

  const id = $props.id();
  let open = $state(false);
  let highlighted = $state(-1);
  let panel = $state<HTMLDivElement>();
  const choice = $derived(options.find((option) => option.value === value));
  let prefix = '';
  let lastKey = 0;

  $effect(() => {
    panel?.focus({ preventScroll: true });
  });

  $effect(() => {
    if (disabled) open = false;
  });

  $effect(() => {
    if (highlighted >= 0) {
      panel?.querySelector(`#${id}-option-${highlighted}`)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    }
  });

  function move(from: number, direction: 1 | -1) {
    const n = options.length;
    for (let step = 1; step <= n; step++) {
      const i = (from + direction * step + n) % n;
      if (!options[i].disabled) return i;
    }
    return -1;
  }

  function show(fromEnd = false) {
    prefix = '';
    lastKey = 0;
    const selected = options.findIndex((option) => option.value === value && !option.disabled);
    highlighted = selected >= 0 ? selected : fromEnd ? move(0, -1) : move(-1, 1);
    open = true;
  }

  function close() {
    trigger?.focus({ preventScroll: true });
    open = false;
  }

  function choose(index: number) {
    const option = options[index];
    if (!option || option.disabled) return;
    close();
    if (value === option.value) return;
    value = option.value;
    onchange?.(value);
  }

  function typeAhead(e: KeyboardEvent) {
    if (e.isComposing || e.key.length !== 1 || e.key === ' ' || e.ctrlKey || e.metaKey || e.altKey) return;
    e.preventDefault();
    e.stopPropagation();
    if (!open) show();
    const key = e.key.toLocaleLowerCase();
    prefix = e.timeStamp - lastKey < 500 ? prefix + key : key;
    lastKey = e.timeStamp;
    // Repeating one key cycles through matches; multiple distinct keys extend the search.
    const repeated = [...prefix].every((character) => character === key);
    const query = repeated ? key : prefix;
    const n = options.length;
    const first = repeated ? 1 : 0;
    for (let step = first; step < n + first; step++) {
      const i = (Math.max(-1, highlighted) + step + n) % n;
      if (!options[i].disabled && options[i].label.toLocaleLowerCase().startsWith(query)) {
        highlighted = i;
        return;
      }
    }
  }

  function triggerKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      e.stopPropagation();
      show(e.key === 'ArrowUp');
    } else typeAhead(e);
  }

  function listKey(e: KeyboardEvent) {
    if (e.key === 'Tab') {
      close();
      return;
    }
    if (!['Escape', 'ArrowDown', 'ArrowUp', 'Home', 'End', 'Enter', ' '].includes(e.key)) {
      typeAhead(e);
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    if (e.key === 'Escape') close();
    else if (e.key === 'Enter' || e.key === ' ') choose(highlighted);
    else if (e.key === 'Home') highlighted = move(-1, 1);
    else if (e.key === 'End') highlighted = move(0, -1);
    else if (e.key === 'ArrowDown') highlighted = move(highlighted, 1);
    else highlighted = move(Math.max(0, highlighted), -1);
  }
</script>

<button
  {...button}
  bind:this={trigger}
  type="button"
  {disabled}
  class="input focus-ring flex cursor-pointer items-center gap-2 text-left disabled:cursor-not-allowed disabled:opacity-40 {size === 'sm' ? 'input-sm' : ''} {cls}"
  aria-label={label}
  aria-labelledby={labelledBy}
  aria-haspopup="listbox"
  aria-expanded={open}
  aria-controls={open ? id : undefined}
  onclick={() => (open ? close() : show())}
  onkeydown={triggerKey}
>
  <span class="min-w-0 flex-1 truncate" class:text-faint={!choice}>{choice?.label ?? placeholder}</span>
  <ChevronsUpDown size={13} class="shrink-0 text-faint" aria-hidden="true" />
</button>

{#if open && trigger}
  <div
    bind:this={panel}
    {id}
    class="popover fixed z-menu w-max overflow-auto p-1 outline-none motion-safe:animate-popover-in"
    role="listbox"
    aria-label={label ?? 'Options'}
    aria-labelledby={labelledBy}
    aria-activedescendant={highlighted >= 0 && options[highlighted] ? `${id}-option-${highlighted}` : undefined}
    tabindex="-1"
    use:placePopup={{ anchor: trigger, matchWidth: true, maxHeight: 320, dismiss: close }}
    onkeydown={listKey}
  >
    {#each options as option, index (option.value)}
      <button
        id={`${id}-option-${index}`}
        type="button"
        role="option"
        tabindex="-1"
        class="menu-item {index === highlighted ? 'bg-hover text-ink' : ''}"
        aria-selected={option.value === value}
        disabled={option.disabled}
        onpointerdown={(e) => e.preventDefault()}
        onclick={() => choose(index)}
      >
        <span class="min-w-0 flex-1 truncate">{option.label}</span>
        {#if option.value === value}<Check size={12} class="shrink-0 text-accent" aria-hidden="true" />{/if}
      </button>
    {/each}
    {#if !options.length}<p class="px-2 py-1 text-sm text-faint">No options</p>{/if}
  </div>
{/if}
