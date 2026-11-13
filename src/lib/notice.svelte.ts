/** How long a notice shows. */
const TOAST_MS = { ok: 5000, okWithAction: 8000, error: 10000 } as const;

/** An outcome that has no other place to show it (a count, an export, a save, an error). */
export interface Notice {
  ok: boolean;
  text: string;
  action?: { label: string; run: () => void };
}

export const notice = $state({ current: null as Notice | null });

let timer: ReturnType<typeof setTimeout> | undefined;

export function notify(ok: boolean, text: string, action?: Notice['action']) {
  notice.current = { ok, text, action };
  clearTimeout(timer);
  timer = setTimeout(() => (notice.current = null), ok ? (action ? TOAST_MS.okWithAction : TOAST_MS.ok) : TOAST_MS.error);
}

export function dismissNotice() {
  clearTimeout(timer);
  notice.current = null;
}
