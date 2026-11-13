<script lang="ts" module>
  /** A production target needs its tick before the action can run; work in flight blocks it too. */
  export const ackBlocked = (prod: boolean, ack: boolean, busy: boolean) => (prod && !ack) || busy;
</script>

<script lang="ts">
  import { ShieldAlert } from '@lucide/svelte';
  import type { Snippet } from 'svelte';

  /** The production acknowledgement; `children` completes the sentence after "This is a production connection. ". */
  let {
    checked = $bindable(false),
    disabled = false,
    class: cls = '',
    children,
  }: { checked?: boolean; disabled?: boolean; class?: string; children: Snippet } = $props();
</script>

<label class="flex cursor-pointer items-start gap-2.5 rounded-lg bg-red-500/8 px-3 py-2.5 text-[12.5px] {cls}">
  <input type="checkbox" class="mt-0.5 accent-red-600" bind:checked {disabled} />
  <span><ShieldAlert size={13} class="mr-0.5 inline text-red-600 dark:text-red-400" /> This is a <b>production</b> connection. {@render children()}</span>
</label>
