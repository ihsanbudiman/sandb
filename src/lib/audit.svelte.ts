import type { Conn } from './connections.svelte';

export type AuditResult =
  | { status: 'ok'; rows: number }
  | { status: 'blocked' | 'denied' }
  | { status: 'error'; message: string };

export interface AuditEntry {
  id: number;
  time: Date;
  connId: string;
  kind: 'READ' | 'WRITE';
  source: 'AI' | 'You';
  sql: string;
  result: AuditResult;
  ms: number;
}

let seq = 0;

export const audit = $state({
  entries: [] as AuditEntry[],
});

/** Logs a statement under the connection it ran on; a run that finishes after a tab switch keeps its own. */
export function logQuery(e: Omit<AuditEntry, 'id' | 'time' | 'connId'> & { conn: Conn }) {
  const { conn, ...rest } = e;
  audit.entries.push({ ...rest, id: ++seq, time: new Date(), connId: conn.id });
}
