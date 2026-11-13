<script lang="ts" module>
  // Open modals, outermost first: Escape and the backdrop belong to the top one only, so a dialog
  // inside a dialog closes alone.
  const stack: number[] = [];
  let seq = 0;
</script>

<script lang="ts">
  import { onMount, type Snippet } from 'svelte';
  import { X } from '@lucide/svelte';

  /**
   * The one dialog frame: backdrop, role, aria, Escape and backdrop policy, header, body and footer.
   * It is mounted only while the dialog is open, so a fresh mount is a fresh dialog.
   * `layout: 'top'` puts a palette near the top; `bare` keeps only the frame for a dialog with its own inside.
   */
  let {
    title,
    label,
    role = 'dialog',
    busy = false,
    error = null,
    errorLabel = null,
    closable = false,
    width = 'max-w-md',
    layout = 'center',
    dismissOnBackdrop = false,
    bare = false,
    class: cls = '',
    bodyClass = '',
    footClass = '',
    onsubmit,
    onclose,
    head,
    children,
    footer,
  }: {
    /** The heading; no header renders without it. */
    title?: string;
    /** `aria-label` for a dialog that has no heading. */
    label?: string;
    role?: 'dialog' | 'alertdialog';
    /** Work in flight: Escape and the close button wait for it. */
    busy?: boolean;
    /** A failure shown above the buttons; format it with `errorText`. */
    error?: string | null;
    /** Bold lead-in of the error, e.g. "Delete failed:". */
    errorLabel?: string | null;
    /** Show the × in the header. */
    closable?: boolean;
    width?: string;
    layout?: 'center' | 'top';
    /** A click outside closes too; only the command palette does, a search surface with no data decision in it. */
    dismissOnBackdrop?: boolean;
    bare?: boolean;
    class?: string;
    bodyClass?: string;
    footClass?: string;
    /** Wrap the frame in a form, so Enter submits from a field. */
    onsubmit?: (e: SubmitEvent) => void;
    onclose: () => void;
    /** Icon before the title. */
    head?: Snippet;
    children: Snippet;
    footer?: Snippet;
  } = $props();

  const id = ++seq;
  stack.push(id);
  onMount(() => () => {
    const at = stack.lastIndexOf(id);
    if (at >= 0) stack.splice(at, 1);
  });

  function close() {
    if (!busy) onclose();
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape' && stack.at(-1) === id) close();
  }
</script>

<svelte:window onkeydown={onkey} />

{#snippet inner()}
  {#if title}
    <header class="dialog-head">
      {@render head?.()}
      <h2 id="dialog-{id}" class="min-w-0 flex-1 truncate">{title}</h2>
      {#if closable}
        <button class="btn btn-ghost -my-1 -mr-1.5 size-7 p-0" onclick={close} disabled={busy} aria-label="Close"><X size={15} /></button>
      {/if}
    </header>
  {/if}
  <div class="dialog-body {bodyClass}">
    {@render children()}
    {#if error}
      <p class="mt-3 break-words rounded-lg bg-red-500/8 px-3 py-2 text-[12.5px] whitespace-pre-line text-red-700 dark:text-red-300" role="alert">
        {#if errorLabel}<b class="font-semibold">{errorLabel}</b> {/if}{error}
      </p>
    {/if}
  </div>
  {#if footer}
    <footer class="dialog-foot {footClass}">{@render footer()}</footer>
  {/if}
{/snippet}

<div class={layout === 'top' ? 'fixed inset-0 z-50 flex items-start justify-center bg-black/35 p-4 pt-[14vh] dark:bg-black/55' : 'dialog-backdrop'}>
  {#if dismissOnBackdrop}
    <button class="absolute inset-0 cursor-default" aria-label="Close" tabindex="-1" onclick={close}></button>
  {/if}
  <div class="dialog {width} {cls}" role={role} aria-labelledby={title ? `dialog-${id}` : undefined} aria-label={title ? undefined : label}>
    {#if bare}
      {@render children()}
    {:else if onsubmit}
      <form {onsubmit}>{@render inner()}</form>
    {:else}
      {@render inner()}
    {/if}
  </div>
</div>
