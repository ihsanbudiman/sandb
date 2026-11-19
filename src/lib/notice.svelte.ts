/** How long a notice shows. */
const TOAST_MS = { ok: 5000, okWithAction: 8000, error: 10000 } as const;

/** An outcome that has no other place to show it (a count, an export, a save, an error). */
export interface Notice {
  id: number;
  ok: boolean;
  text: string;
  action?: { label: string; run: () => void };
}

const LIMIT = 3;
export const notice = $state({ list: [] as Notice[] });

let seq = 0;
const timers = new Map<number, ReturnType<typeof setTimeout>>();

export function notify(ok: boolean, text: string, action?: Notice['action']) {
  if (notice.list.length === LIMIT) dismissNotice(notice.list[0].id);
  const id = ++seq;
  notice.list.push({ id, ok, text, action });
  timers.set(id, setTimeout(() => dismissNotice(id), ok ? (action ? TOAST_MS.okWithAction : TOAST_MS.ok) : TOAST_MS.error));
}

export function dismissNotice(id: number) {
  clearTimeout(timers.get(id));
  timers.delete(id);
  const i = notice.list.findIndex((n) => n.id === id);
  if (i >= 0) notice.list.splice(i, 1);
}
