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
    describedBy,
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
    /** ID of text that explains the dialog's purpose or decision. */
    describedBy?: string;
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

  const focusable = 'a[href], button, input:not([type="hidden"]), select, textarea, summary, [contenteditable="true"], [tabindex]';
  const previous = document.activeElement;
  let frame: HTMLDivElement;

  const id = ++seq;
  stack.push(id);
  onMount(() => {
    if (stack.at(-1) === id) focusInside();
    return () => {
      const at = stack.lastIndexOf(id);
      const wasTop = at === stack.length - 1;
      if (at >= 0) stack.splice(at, 1);
      if (wasTop && previous instanceof HTMLElement && previous.isConnected) {
        previous.focus({ preventScroll: true });
      }
    };
  });

  function available(el: HTMLElement) {
    return !el.matches(':disabled') && !el.closest('[inert]') &&
      el.getClientRects().length > 0 && getComputedStyle(el).visibility === 'visible';
  }

  function tabStops() {
    return Array.from(frame.querySelectorAll<HTMLElement>(focusable)).filter((el) => el.tabIndex >= 0 && available(el));
  }

  function focusInside() {
    const preferred = frame.querySelector<HTMLElement>('[data-autofocus]');
    const target = preferred && preferred.matches(focusable) && available(preferred) ? preferred : tabStops()[0] ?? frame;
    target.focus({ preventScroll: true });
  }

  function close() {
    if (!busy) onclose();
  }

  function onkey(e: KeyboardEvent) {
    if (e.defaultPrevented || stack.at(-1) !== id) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
    } else if (e.key === 'Tab') {
      const stops = tabStops();
      const at = stops.findIndex((el) => el === document.activeElement);
      if (at === -1 || at === (e.shiftKey ? 0 : stops.length - 1)) {
        e.preventDefault();
        (stops[e.shiftKey ? stops.length - 1 : 0] ?? frame).focus();
      }
    }
  }
</script>

<svelte:window onkeydown={onkey} />

{#snippet inner()}
  {#if title}
    <header class="dialog-head">
      {@render head?.()}
      <h2 id="dialog-{id}" class="min-w-0 flex-1 truncate">{title}</h2>
      {#if closable}
        <button type="button" class="btn btn-ghost -my-1 -mr-1.5 size-7 p-0" onclick={close} disabled={busy} title="Close" aria-label="Close"><X size={15} /></button>
      {/if}
    </header>
  {/if}
  <div class="dialog-body {bodyClass}">
    {@render children()}
    {#if error}
      <p class="mt-3 break-words rounded-lg bg-danger-soft px-3 py-2 text-sm whitespace-pre-line text-danger" role="alert">
        {#if errorLabel}<b class="font-semibold">{errorLabel}</b> {/if}{error}
      </p>
    {/if}
  </div>
  {#if footer}
    <footer class="dialog-foot {footClass}">{@render footer()}</footer>
  {/if}
{/snippet}

<div class="motion-safe:animate-dialog-backdrop-in {layout === 'top' ? 'fixed inset-0 z-dialog flex items-start justify-center bg-black/35 p-4 pt-[14vh] dark:bg-black/55' : 'dialog-backdrop'}">
  {#if dismissOnBackdrop}
    <button class="absolute inset-0 cursor-default" aria-label="Close" tabindex="-1" onclick={close}></button>
  {/if}
  <div bind:this={frame} class="dialog motion-safe:animate-dialog-in {width} {cls}" role={role} aria-modal="true" aria-labelledby={title ? `dialog-${id}` : undefined} aria-label={title ? undefined : label} aria-describedby={describedBy} tabindex="-1">
    {#if bare}
      {@render children()}
    {:else if onsubmit}
      <form {onsubmit}>{@render inner()}</form>
    {:else}
      {@render inner()}
    {/if}
  </div>
</div>
