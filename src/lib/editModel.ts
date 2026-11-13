import type { Row, TypedValue } from './api';

/** Pending grid edits of one tab. Keys of `changed`/`deleted` are row indexes in the tab's shown result. */
export interface Edits {
  on: boolean;
  /** The new cell values; on MongoDB each one is a `TypedValue`. */
  changed: Record<number, Row>;
  /** On MongoDB `doc` is the new document as mongosh text; its grid row stays all NULL. */
  added: { key: number; row: Row; doc?: string }[];
  deleted: Record<number, true>;
  error: { message: string; key: string } | null;
  /** A save is in flight; the tab cannot close until it ends. */
  saving: boolean;
}

export const newEdits = (): Edits => ({ on: false, changed: {}, added: [], deleted: {}, error: null, saving: false });

export function pendingCount(e: Edits) {
  const del = Object.keys(e.deleted).length;
  const upd = Object.keys(e.changed).filter((i) => !e.deleted[+i]).length;
  return upd + e.added.length + del;
}

export function resetEdits(e: Edits, keepOn = false) {
  e.changed = {};
  e.added = [];
  e.deleted = {};
  e.error = null;
  if (!keepOn) e.on = false;
}

export interface Stmt {
  kind: 'UPDATE' | 'INSERT' | 'DELETE';
  /** SQL, or the mongosh command on MongoDB. */
  sql: string;
  /** Grid row key: `r<index>` for result rows, `a<key>` for new rows. */
  key: string;
  pk: Row;
  set: Row;
  /** MongoDB update: the old value of each changed path. */
  old?: Record<string, TypedValue>;
  document?: string;
}
