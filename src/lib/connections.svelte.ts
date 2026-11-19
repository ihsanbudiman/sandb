import { api, ApiError, type Connection, type Engine, type Env } from './api';
import { notify } from './notice.svelte';
import type { SqlEngine } from './sql/text';

export type { Engine, Env };

/** A saved connection plus UI-only fields. `aiWrite`: AI may propose writes (each still needs approval). */
export interface Conn extends Connection {
  /** Set on an unsaved duplicate: the id of the connection it copies (and whose password it reuses). */
  copyOf?: string;
}

export const engines: Record<Engine, { label: string; short: string; badge: string; port: number }> = {
  pg: { label: 'PostgreSQL', short: 'PG', badge: 'text-sky-700 dark:text-sky-400', port: 5432 },
  mysql: { label: 'MySQL / MariaDB', short: 'MY', badge: 'text-orange-800 dark:text-orange-400', port: 3306 },
  sqlite: { label: 'SQLite', short: 'SL', badge: 'text-violet-700 dark:text-violet-400', port: 0 },
  mongo: { label: 'MongoDB', short: 'MG', badge: 'text-ok', port: 27017 },
  redis: { label: 'Redis', short: 'RD', badge: 'text-red-700 dark:text-red-400', port: 6379 },
};

/** True for engines that support the SQL-only controls. */
export const isSql = (c: { engine: Engine }): c is { engine: SqlEngine } => c.engine !== 'mongo' && c.engine !== 'redis';
export const isMongo = (c: { engine: Engine }) => c.engine === 'mongo';
export const isRedis = (c: { engine: Engine }) => c.engine === 'redis';

export const envDot: Record<Env, string> = {
  // The ring tells production from staging without relying on hue.
  production: 'bg-danger ring-2 ring-danger/35',
  staging: 'bg-warn',
  local: 'bg-stone-500 dark:bg-stone-400',
};

export const envTag: Record<Env, string> = {
  production: 'text-danger',
  staging: 'text-warn',
  local: 'text-muted',
};

export const db = $state({
  connections: [] as Conn[],
  loading: true,
  loadError: '',
});

export const defaultDb = (c: { engine: Engine; database?: string | null }) => (c.engine === 'sqlite' ? 'main' : (c.database ?? ''));

export async function loadConnections() {
  db.loading = true;
  db.loadError = '';
  try {
    db.connections = await api.listConnections();
  } catch (e) {
    db.loadError = ApiError.from(e).message;
  } finally {
    db.loading = false;
  }
}

/** One database a chat can use: a connection, and the database on it (`null`: the connection's own). */
export interface Source {
  connId: string;
  database: string | null;
}

export const connOf = (s: Source): Conn | undefined => db.connections.find((c) => c.id === s.connId);
export const dbOf = (s: Source, c = connOf(s)) => s.database ?? (c ? defaultDb(c) : '');
export const sameSource = (a: Source, b: Source) => a.connId === b.connId && dbOf(a) === dbOf(b);

/** False on a read-only connection: no write UI (editor, AI, edit mode) may act on it. */
export function canWrite(conn: { readOnly: boolean }): boolean {
  return !conn.readOnly;
}

/** Where a connection points: the backend's `same_target` compares the same fields, and not the database. */
type Target = Partial<Pick<Connection, 'engine' | 'host' | 'port' | 'user' | 'file' | 'uri' | 'redis'>>;
const targetField = (c: Target) => (c.engine === 'sqlite' ? { host: '', port: 0, user: '' } : { host: c.host ?? '', port: c.port ?? 0, user: c.user ?? '' });

/** True when both point at the same server and database file. SQLite's unused host, port and user do not count. */
export function sameTarget(a: Target, b: Target): boolean {
  const [x, y] = [targetField(a), targetField(b)];
  return a.engine === b.engine && x.host === y.host && x.port === y.port && x.user === y.user && (a.file ?? '') === (b.file ?? '') && (a.uri ?? '') === (b.uri ?? '') &&
    JSON.stringify(a.redis ?? null) === JSON.stringify(b.redis ?? null);
}

/**
 * Saves the AI write mode of a stored connection; the store changes only after the backend took it,
 * so the write UI never claims a mode the backend does not hold. A failure is shown, not thrown.
 */
export async function setAiWrite(c: Conn, on: boolean) {
  try {
    const saved = await api.updateConnection({ id: c.id, input: { ...c, aiWrite: on }, password: null });
    const stored = db.connections.find((x) => x.id === c.id);
    if (stored) Object.assign(stored, saved);
  } catch (e) {
    notify(false, `The AI write setting was not saved: ${ApiError.from(e).message}`);
  }
}

export function duplicateDraft(c: Conn): Conn {
  const name = `${c.name} (copy)`;
  return { ...c, id: name.toLowerCase().replace(/[^a-z0-9]+/g, '-') + '-' + Date.now().toString(36), name, aiWrite: false, copyOf: c.id };
}
