<script lang="ts">
  import { onMount, type Snippet } from 'svelte';
  import { placePopup, type PopupAnchor } from './popup';

  let { anchor, label, align = 'start', returnFocus, onclose, children, class: cls = '', id }: {
    anchor: PopupAnchor;
    label: string;
    align?: 'start' | 'end';
    returnFocus?: HTMLElement | null;
    onclose: () => void;
    children: Snippet;
    class?: string;
    id?: string;
  } = $props();

  const selector = '[role="menuitem"], [role="menuitemradio"], [role="menuitemcheckbox"]';
  let panel: HTMLDivElement;
  let opener: HTMLElement | null = null;

  function items() {
    return Array.from(panel.querySelectorAll<HTMLElement>(selector)).filter((item) =>
      !item.matches(':disabled, [aria-disabled="true"]') && item.getClientRects().length > 0,
    );
  }

  onMount(() => {
    opener = anchor instanceof HTMLElement ? anchor : document.activeElement instanceof HTMLElement ? document.activeElement : null;
    (items()[0] ?? panel).focus();
    // An async menu can render its first actions after opening on a loading message.
    const observer = new MutationObserver(() => {
      if (document.activeElement === panel) items()[0]?.focus();
    });
    observer.observe(panel, { childList: true, subtree: true, attributes: true, attributeFilter: ['disabled', 'aria-disabled'] });
    return () => {
      observer.disconnect();
      if (panel.contains(document.activeElement)) restoreFocus();
    };
  });

  function restoreFocus() {
    (returnFocus === undefined ? opener : returnFocus)?.focus({ preventScroll: true });
  }

  function close() {
    restoreFocus();
    onclose();
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Tab') {
      // Return to the trigger so the browser tabs onward from it, not from the floating menu.
      close();
      return;
    }
    if (!['Escape', 'ArrowDown', 'ArrowUp', 'Home', 'End', 'Enter', ' '].includes(e.key)) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === 'Escape') { close(); return; }

    const rows = items();
    if (!rows.length) return;
    const current = rows.findIndex((item) => item === document.activeElement);
    if (e.key === 'Enter' || e.key === ' ') rows[current]?.click();
    else if (e.key === 'Home') rows[0].focus();
    else if (e.key === 'End') rows[rows.length - 1].focus();
    else if (e.key === 'ArrowDown') rows[(current + 1) % rows.length].focus();
    else rows[current <= 0 ? rows.length - 1 : current - 1].focus();
  }

  function activate(e: MouseEvent) {
    const item = e.target instanceof Element ? e.target.closest(selector) : null;
    if (!item) return;
    if (item.matches(':disabled, [aria-disabled="true"]')) {
      e.preventDefault();
      e.stopPropagation();
    } else {
      // Restore before an action opens another focus-managed surface (e.g. a confirm).
      close();
    }
  }
</script>

<div
  bind:this={panel}
  {id}
  class="popover fixed z-menu w-44 overflow-auto p-1 motion-safe:animate-popover-in {cls}"
  role="menu"
  aria-label={label}
  tabindex="-1"
  use:placePopup={{ anchor, align, dismiss: close }}
  onkeydown={key}
  onclickcapture={activate}
>
  {@render children()}
</div>
