<script lang="ts" module>
  /** A production target needs its tick before the action can run; work in flight blocks it too. */
  export const ackBlocked = (prod: boolean, ack: boolean, busy: boolean) => (prod && !ack) || busy;
</script>

<script lang="ts">
  import { ShieldAlert } from '@lucide/svelte';
  import type { Snippet } from 'svelte';

  /** The production acknowledgement; `children` completes the sentence after the protection notice. */
  let {
    checked = $bindable(false),
    disabled = false,
    class: cls = '',
    children,
  }: { checked?: boolean; disabled?: boolean; class?: string; children: Snippet } = $props();
</script>

<label class="flex cursor-pointer items-start gap-2.5 rounded-lg bg-danger-soft px-3 py-2.5 text-sm {cls}">
  <input type="checkbox" class="mt-0.5 accent-danger" bind:checked {disabled} />
  <span><ShieldAlert size={13} class="mr-0.5 inline text-danger" /> <b>Write protection</b> is on for this connection. {@render children()}</span>
</label>
