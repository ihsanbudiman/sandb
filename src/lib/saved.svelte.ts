import { autosave, readStored } from './persist.svelte';
import { tabs, type Tab } from './tabs.svelte';

export interface SavedQuery {
  id: string;
  name: string;
  sql: string;
  connId: string;
  database: string;
  created: number;
  updated: number;
}

const KEY = 'sandb.saved-queries.v1';

export const saved = $state({ list: readStored<SavedQuery[]>(KEY, [], { migrate: (data) => (Array.isArray(data) ? (data as SavedQuery[]) : []) }) });

autosave(KEY, () => saved.list, { delay: 200, onError: (e) => console.warn('Saved queries were not stored.', e) });

export const findSaved = (id: string | null | undefined) => (id ? saved.list.find((q) => q.id === id) : undefined);

export function isDirty(t: Tab): boolean {
  const q = findSaved(t.savedId);
  return !!q && q.sql !== t.query;
}

export function suggestName(sql: string): string {
  const clean = sql.replace(/--.*$/gm, '').replace(/\/\*[\s\S]*?\*\//g, '');
  const m = /\b(?:from|into|update|join|table)\s+([`"[\]\w.]+)/i.exec(clean);
  const name = m?.[1].replace(/[`"[\]]/g, '').split('.').pop();
  return name ? `${name} query` : 'Untitled query';
}

export function createSaved(t: Tab, name: string): SavedQuery {
  const now = Date.now();
  saved.list.push({ id: crypto.randomUUID(), name, sql: t.query, connId: t.connId, database: t.database, created: now, updated: now });
  const q = saved.list[saved.list.length - 1];
  t.savedId = q.id;
  t.title = name;
  return q;
}

export function updateSaved(t: Tab) {
  const q = findSaved(t.savedId);
  if (!q) return;
  q.sql = t.query;
  q.connId = t.connId;
  q.database = t.database;
  q.updated = Date.now();
}

/** Linked tabs take the new name too. */
export function renameSaved(id: string, name: string) {
  const q = findSaved(id);
  if (!q || !name.trim()) return;
  q.name = name.trim();
  for (const t of tabs.list) if (t.savedId === id) t.title = q.name;
}

export function duplicateSaved(id: string): SavedQuery | undefined {
  const q = findSaved(id);
  if (!q) return;
  const now = Date.now();
  saved.list.push({ ...q, id: crypto.randomUUID(), name: `${q.name} copy`, created: now, updated: now });
  return saved.list[saved.list.length - 1];
}

export function deleteSaved(id: string): () => void {
  const i = saved.list.findIndex((q) => q.id === id);
  if (i < 0) return () => {};
  const [q] = saved.list.splice(i, 1);
  const linked = tabs.list.filter((t) => t.savedId === id).map((t) => t.id);
  for (const t of tabs.list) if (t.savedId === id) t.savedId = null;
  return () => {
    if (findSaved(q.id)) return;
    saved.list.splice(Math.min(i, saved.list.length), 0, q);
    for (const t of tabs.list) if (linked.includes(t.id) && !t.savedId) t.savedId = q.id;
  };
}
