/** Element actions for focus and scroll that must happen after the DOM has the element. */

export function selectOnMount(el: HTMLInputElement) {
  el.focus();
  el.select();
}

export function focusOn(el: HTMLElement, token: number) {
  if (token) el.focus({ preventScroll: true });
}

export function scrollOn(el: HTMLElement, on: boolean) {
  if (on) el.scrollIntoView({ block: 'nearest' });
}

export function focusFirst(el: HTMLElement) {
  el.querySelector<HTMLElement>('[role="menuitem"]:not([disabled])')?.focus();
}
