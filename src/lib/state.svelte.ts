import { api, ApiError, type AiSettings, type Connection, type Engine, type Env } from './api';
import { seedConnections } from './api/mock';
import { tabs, activeTab, setTabConn } from './tabs.svelte';

export type { Engine, Env };

/** A saved connection (see api/types.ts) plus UI-only fields. `aiWrite`: AI may propose writes (each still needs approval). */
export interface Conn extends Connection {
  /** Key into the mock schemas in mock.ts. Schema contents stay mock until introspection exists. */
  schema: string;
  /** Set on an unsaved duplicate: the id of the connection it copies (and whose password it reuses). */
  copyOf?: string;
}

// MongoDB has no mock schema: no SQL view opens on it.
const mockSchemaOf: Record<Engine, string> = { pg: 'prod', mysql: 'shop', sqlite: 'notes', mongo: '' };
export const toConn = (c: Connection): Conn => ({ ...c, schema: mockSchemaOf[c.engine] });

export const engines: Record<Engine, { label: string; short: string; badge: string; port: number }> = {
  pg: { label: 'PostgreSQL', short: 'PG', badge: 'text-sky-700 dark:text-sky-400', port: 5432 },
  mysql: { label: 'MySQL / MariaDB', short: 'MY', badge: 'text-orange-800 dark:text-orange-400', port: 3306 },
  sqlite: { label: 'SQLite', short: 'SL', badge: 'text-violet-700 dark:text-violet-400', port: 0 },
  mongo: { label: 'MongoDB', short: 'MG', badge: 'text-emerald-700 dark:text-emerald-400', port: 27017 },
};

/** True for the SQL engines. MongoDB connections open the editor and the chat too, but stay out of the SQL-only features (schema views, per-statement write previews, AI write settings). */
export const isSql = (c: { engine: Engine }) => c.engine !== 'mongo';

export const envDot: Record<Env, string> = {
  // The ring tells production from staging without relying on hue.
  production: 'bg-red-600 ring-2 ring-red-600/30 dark:bg-red-500 dark:ring-red-500/35',
  staging: 'bg-amber-700 dark:bg-amber-500',
  local: 'bg-stone-500 dark:bg-stone-400',
};

export const envTag: Record<Env, string> = {
  production: 'text-red-700 dark:text-red-300',
  staging: 'text-amber-700 dark:text-amber-400',
  local: 'text-muted',
};

/** Bumped after a structure change (DDL, an index/collection change), so open Properties views reload. */
export const structure = $state({ rev: 0 });
export const structureChanged = () => structure.rev++;

export const ui = $state({
  dark: false,
  chatOpen: true,
  /** Assistant panel width in px. */
  chatWidth: 340,
  page: 'query' as 'query' | 'settings' | 'audit',
  /** The Settings tab that shows. */
  settingsSection: 'ai' as 'ai' | 'safeguards' | 'appearance' | 'updates',
  theme: 'light' as 'light' | 'dark' | 'system',
  /** Sidebar width in px; the title bar uses it too, so the tabs stay in line with the main card. */
  sidebarWidth: 224,
  /** False hides the sidebar; the main card then starts at the window edge. */
  sidebarOpen: true,
  /** What the sidebar lists: the connection tree or the saved queries. */
  sidebarView: 'connections' as 'connections' | 'queries',
  /** Editor height as a fraction of the main card; the results grid takes the rest. */
  editorSplit: 0.34,
  /** False hides the results grid; the editor then fills the card. */
  resultsOpen: true,
});

/** The wire format of the AI server. Any other saved label loads as `openai`. */
export type Provider = 'openai' | 'anthropic';

export const providers: Record<Provider, { label: string; url: string; model: string }> = {
  openai: { label: 'OpenAI-compatible', url: 'https://api.openai.com/v1', model: 'gpt-4.1-mini' },
  anthropic: { label: 'Anthropic-compatible', url: 'https://api.anthropic.com/v1', model: 'claude-sonnet-4-5' },
};

export const ai = $state({
  provider: 'openai' as Provider,
  baseUrl: 'http://localhost:11434/v1',
  apiKey: '',
  model: 'llama3.1:8b',
  /** 'results' = schema + query results go to the model; 'schema' = schema only. */
  privacy: 'results' as 'results' | 'schema',
  maxRows: 500,
  timeoutSec: 15,
  /** A key is stored. The stored key never comes back to the UI; `apiKey` holds only what the user typed. */
  hasKey: false,
});

// Mock mode starts with the seed synchronously, exactly like the prototype; real backends load on start.
const isMock = api.kind === 'mock';

export const db = $state({
  connections: (isMock ? seedConnections.map(toConn) : []) as Conn[],
  loading: !isMock,
  loadError: '',
});

export async function loadConnections() {
  if (isMock) return;
  db.loading = true;
  db.loadError = '';
  try {
    db.connections = (await api.listConnections()).map(toConn);
    // Tabs opened before the load (or on a connection that is gone) move to the first connection.
    const first = db.connections[0];
    if (first) for (const t of tabs.list) if (!db.connections.some((c) => c.id === t.connId)) setTabConn(t, first);
  } catch (e) {
    db.loadError = ApiError.from(e).message;
  } finally {
    db.loading = false;
  }
}

/** The saved AI settings fill `ai`. The mock keeps the defaults. */
export async function loadAiSettings() {
  if (isMock) return;
  try {
    const s = await api.getAiSettings();
    ai.provider = s.provider === 'anthropic' ? 'anthropic' : 'openai';
    ai.baseUrl = s.baseUrl;
    ai.model = s.model;
    ai.privacy = s.privacy;
    ai.maxRows = s.maxRows;
    ai.timeoutSec = s.timeoutSec;
    ai.hasKey = s.hasKey;
  } catch (e) {
    console.warn('AI settings were not loaded; using the defaults.', ApiError.from(e).message);
  }
}

/** The AI settings without the key, as the backend takes them. */
export const aiSettings = (): AiSettings => ({ provider: ai.provider, baseUrl: ai.baseUrl, model: ai.model, privacy: ai.privacy, maxRows: ai.maxRows, timeoutSec: ai.timeoutSec });

// Edits are saved shortly after the user stops typing. One queue keeps the writes in order.
const AI_SAVE_MS = 500;
let settingsDirty = false;
let keyDirty = false;
let settingsTimer: ReturnType<typeof setTimeout> | undefined;
let keyTimer: ReturnType<typeof setTimeout> | undefined;
let aiQueue: Promise<unknown> = Promise.resolve();

function enqueue<T>(f: () => Promise<T>): Promise<T> {
  const p = aiQueue.then(f);
  aiQueue = p.catch(() => {});
  return p;
}

async function writeSettings() {
  clearTimeout(settingsTimer);
  if (!settingsDirty) return;
  settingsDirty = false;
  try {
    ai.hasKey = (await api.saveAiSettings(aiSettings())).hasKey;
  } catch (e) {
    settingsDirty = true;
    throw e;
  }
}

// An empty input keeps the stored key; only removeAiKey deletes it.
async function writeKey() {
  clearTimeout(keyTimer);
  if (!keyDirty) return;
  keyDirty = false;
  const key = ai.apiKey.trim();
  if (!key) return;
  try {
    await api.setAiKey(key);
    ai.hasKey = true;
  } catch (e) {
    keyDirty = true;
    throw e;
  }
}

const warnUnsaved = (e: unknown) => console.warn('AI settings were not saved.', ApiError.from(e).message);

/** Saves the AI settings (not the key) after a short pause. */
export function saveAiSoon() {
  settingsDirty = true;
  clearTimeout(settingsTimer);
  settingsTimer = setTimeout(() => enqueue(writeSettings).catch(warnUnsaved), AI_SAVE_MS);
}

/** Stores the typed key after a short pause. */
export function saveAiKeySoon() {
  keyDirty = true;
  clearTimeout(keyTimer);
  keyTimer = setTimeout(() => enqueue(writeKey).catch(warnUnsaved), AI_SAVE_MS);
}

/** Writes pending settings and key now. Fails with the backend error, for example `invalid_input`. */
export function flushAi(): Promise<void> {
  return enqueue(async () => {
    await writeSettings();
    await writeKey();
  });
}

/** Deletes the stored key and clears the input. */
export async function removeAiKey() {
  keyDirty = false;
  clearTimeout(keyTimer);
  ai.apiKey = '';
  await enqueue(() => api.setAiKey(null));
  ai.hasKey = false;
}

/** Models the AI server lists. `target`: the provider, URL and key that the list is for. */
export const aiModels = $state({ list: [] as string[], loading: false, error: '', target: '' });
let modelsSeq = 0;

/** Loads the model list unless it is already loaded (or loading) for the current provider, URL and key. `force` loads again. */
export async function loadAiModels(force = false) {
  const target = [ai.provider, ai.baseUrl.trim().replace(/\/+$/, ''), ai.apiKey.trim() || (ai.hasKey ? '(saved)' : '')].join('\n');
  if (!force && aiModels.target === target && (aiModels.loading || (aiModels.list.length && !aiModels.error))) return;
  const seq = ++modelsSeq;
  if (aiModels.target !== target) aiModels.list = [];
  aiModels.target = target;
  aiModels.loading = true;
  aiModels.error = '';
  try {
    const list = await api.listAiModels(aiSettings(), ai.apiKey.trim() || null);
    if (seq === modelsSeq) aiModels.list = list;
  } catch (e) {
    if (seq === modelsSeq) aiModels.error = ApiError.from(e).message;
  } finally {
    if (seq === modelsSeq) aiModels.loading = false;
  }
}

/** Chooses the model for every chat and saves it. */
export function setAiModel(model: string) {
  ai.model = model.trim();
  saveAiSoon();
}

// Stands in while there are no connections, so views that read active() keep working.
const placeholder: Conn = { id: '', name: '', engine: 'pg', version: '', env: 'local', host: '', port: 0, database: '', user: '', ssl: 'prefer', file: '', aiWrite: false, readOnly: false, uri: '', authSource: '', topology: null, schema: 'prod' };

/** The active tab's connection, whatever its engine. */
export function active(): Conn {
  const id = activeTab()?.connId;
  return db.connections.find((c) => c.id === id) ?? db.connections[0] ?? placeholder;
}

/** False on a read-only connection: no write UI (editor, AI, edit mode) may act on it. */
export function canWrite(conn: Conn | Connection): boolean {
  return !conn.readOnly;
}
