<script lang="ts">
  import type { HTMLButtonAttributes } from 'svelte/elements';

  let {
    checked = $bindable(false),
    size = 'md',
    tone = 'accent',
    onchange,
    class: cls = '',
    ...button
  }: Omit<HTMLButtonAttributes, 'children' | 'onclick' | 'onchange' | 'type' | 'role' | 'aria-checked'> & {
    checked?: boolean;
    size?: 'md' | 'sm';
    tone?: 'accent' | 'warn';
    onchange?: (checked: boolean) => void;
  } = $props();

  function toggle() {
    if (onchange) onchange(!checked);
    else checked = !checked;
  }
</script>

<button
  {...button}
  type="button"
  role="switch"
  aria-checked={checked}
  class="focus-ring relative shrink-0 cursor-pointer rounded-full motion-safe:transition-colors motion-safe:duration-150 disabled:cursor-not-allowed disabled:opacity-40 {size === 'sm' ? 'h-4 w-7' : 'h-5 w-9'} {checked ? (tone === 'warn' ? 'bg-warn' : 'bg-accent') : 'bg-faint'} {cls}"
  onclick={toggle}
>
  <span class="absolute top-0.5 left-0.5 rounded-full bg-white shadow motion-safe:transition-transform motion-safe:duration-150 {size === 'sm' ? 'size-3' : 'size-4'} {checked ? (size === 'sm' ? 'translate-x-3' : 'translate-x-4') : ''}"></span>
</button>
