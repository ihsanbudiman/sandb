import { ApiError } from '../types';
import type { AiSettings, Connection } from '../generated/wire';
import { DEFAULT_AI_BASE_URL, DEFAULT_AI_MODEL, DEFAULT_MAX_ROWS, DEFAULT_TIMEOUT_SEC } from '../generated/constants';
import { seedConnections } from './data';

let list: Connection[] = seedConnections.map((c) => ({ ...c }));

/** Tab id → connection id: only writable Production connections open a session. */
export const sessions = new Map<string, string>();

export const all = () => list;

export function find(id: string): Connection {
  const c = list.find((x) => x.id === id);
  if (!c) throw new ApiError({ code: 'not_found', message: `Connection ${id} not found` });
  return c;
}

export function add(c: Connection) {
  list.push(c);
}

export function replace(c: Connection) {
  list = list.map((x) => (x.id === c.id ? c : x));
  dropSessionsOf(c.id);
}

export function remove(id: string) {
  list = list.filter((x) => x.id !== id);
  dropSessionsOf(id);
}

export function reorder(ids: string[]) {
  const next = ids.map((id) => list.find((c) => c.id === id));
  if (new Set(ids).size !== list.length || next.length !== list.length || next.some((c) => !c)) {
    throw new ApiError({ code: 'invalid_input', message: 'The connection order is out of date' });
  }
  list = next as Connection[];
}

export function dropSessionsOf(connId: string) {
  for (const [tab, id] of sessions) if (id === connId) sessions.delete(tab);
}

// The demo build keeps the AI settings in memory and never calls a model.
export const ai = {
  settings: { provider: 'openai', baseUrl: DEFAULT_AI_BASE_URL, model: DEFAULT_AI_MODEL, privacy: 'results', maxRows: DEFAULT_MAX_ROWS, timeoutSec: DEFAULT_TIMEOUT_SEC } as AiSettings,
  key: '',
};
