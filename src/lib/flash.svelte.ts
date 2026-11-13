/** How long the "copied" state shows. */
export const FLASH_MS = 1200;

export class Flash<T> {
  current = $state<T | null>(null);
  timer: ReturnType<typeof setTimeout> | undefined;

  show(value: T) {
    this.current = value;
    clearTimeout(this.timer);
    this.timer = setTimeout(() => (this.current = null), FLASH_MS);
  }
}

/** The same one-shot clock for state outside Svelte, such as an icon inside sanitized HTML. */
export function flashOnce(restore: () => void) {
  setTimeout(restore, FLASH_MS);
}
