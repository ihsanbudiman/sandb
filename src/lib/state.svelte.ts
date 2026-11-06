import { api, ApiError, type AiSettings, type ChatGptModel, type ChatGptStatus, type AppErrorBody, type Connection, type Engine, type Env } from './api';
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

const mockSchemaOf: Record<Engine, string> = { pg: 'prod', mysql: 'shop', sqlite: 'notes' };
export const toConn = (c: Connection): Conn => ({ ...c, schema: mockSchemaOf[c.engine] });

export const engines: Record<Engine, { label: string; short: string; badge: string; port: number }> = {
  pg: { label: 'PostgreSQL', short: 'PG', badge: 'text-sky-700 dark:text-sky-400', port: 5432 },
  mysql: { label: 'MySQL / MariaDB', short: 'MY', badge: 'text-orange-800 dark:text-orange-400', port: 3306 },
  sqlite: { label: 'SQLite', short: 'SL', badge: 'text-violet-700 dark:text-violet-400', port: 0 },
};

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

/** API-key transports and the additional OAuth ChatGPT plan option. */
export type Provider = 'openai' | 'anthropic' | 'chatgpt';

export const providers: Record<Provider, { label: string; url: string; model: string }> = {
  openai: { label: 'OpenAI-compatible', url: 'https://api.openai.com/v1', model: 'gpt-4.1-mini' },
  anthropic: { label: 'Anthropic-compatible', url: 'https://api.anthropic.com/v1', model: 'claude-sonnet-4-5' },
  chatgpt: { label: 'ChatGPT plan', url: '', model: '' },
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
    ai.provider = s.provider === 'chatgpt' ? 'chatgpt' : s.provider === 'anthropic' ? 'anthropic' : 'openai';
    ai.baseUrl = s.baseUrl;
    ai.model = s.model;
    ai.privacy = s.privacy;
    ai.maxRows = s.maxRows;
    ai.timeoutSec = s.timeoutSec;
    ai.hasKey = s.hasKey;
    invalidateAiModels();
    await loadChatGptStatus();
    if (ai.provider === 'chatgpt') await loadAiModels();
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

export const chatgpt = $state({
  supported: api.kind === 'tauri', signedIn: false, accountLabel: null as string | null,
  usageEnabled: false, loading: false, pending: false, cancelling: false, signingOut: false,
  welcome: false, usagePaused: false, error: null as ApiError | null, warning: '',
});
let authSeq = 0;
let accountRevision = 0;
const WELCOME_KEY = 'sandb.chatgpt.welcomed';
let welcomed = false;

export const aiModels = $state({ list: [] as string[], catalog: [] as ChatGptModel[], loading: false, error: '', target: '' });
let modelsSeq = 0;

function invalidateAiModels() {
  modelsSeq++;
  aiModels.list = [];
  aiModels.catalog = [];
  aiModels.target = aiModels.error = '';
  aiModels.loading = false;
}

function applyChatGptStatus(status: ChatGptStatus) {
  if (chatgpt.signedIn !== status.signedIn || chatgpt.accountLabel !== status.accountLabel || chatgpt.usageEnabled !== status.usageEnabled) {
    accountRevision++;
    invalidateAiModels();
  }
  chatgpt.signedIn = status.signedIn;
  chatgpt.accountLabel = status.accountLabel;
  chatgpt.usageEnabled = status.usageEnabled;
}

export async function loadChatGptStatus() {
  const seq = authSeq;
  chatgpt.loading = true;
  try {
    const status = await api.chatgptStatus();
    if (seq === authSeq) applyChatGptStatus(status);
  } catch (e) {
    if (seq === authSeq) chatgpt.error = ApiError.from(e);
  } finally {
    if (seq === authSeq) chatgpt.loading = false;
  }
}

export async function signInChatGpt(enableUsage = false) {
  if (chatgpt.pending || chatgpt.signingOut) return;
  const seq = ++authSeq;
  chatgpt.pending = true;
  chatgpt.cancelling = false;
  chatgpt.loading = false;
  chatgpt.error = null;
  chatgpt.warning = '';
  accountRevision++;
  invalidateAiModels();
  try {
    const status = await api.chatgptSignIn(enableUsage);
    if (seq !== authSeq) return;
    applyChatGptStatus(status);
    chatgpt.usagePaused = false;
    if (status.signedIn && status.usageEnabled) {
      try { welcomed ||= localStorage.getItem(WELCOME_KEY) === 'yes'; } catch { /* Storage may be disabled. */ }
      if (!welcomed) {
        welcomed = true;
        chatgpt.welcome = true;
        ui.settingsSection = 'ai';
        ui.page = 'settings';
        try { localStorage.setItem(WELCOME_KEY, 'yes'); } catch { /* The in-memory flag still prevents repeats. */ }
      }
    }
    chatgpt.pending = false;
    if (ai.provider === 'chatgpt') await loadAiModels(true);
  } catch (e) {
    if (seq === authSeq) {
      const error = ApiError.from(e);
      chatgpt.pending = false;
      await loadChatGptStatus();
      if (seq !== authSeq) return;
      if (ai.provider === 'chatgpt') await loadAiModels(true);
      if (seq === authSeq && error.code !== 'cancelled') chatgpt.error = error;
    }
  } finally {
    if (seq === authSeq) chatgpt.pending = false;
  }
}

export async function cancelChatGptSignIn() {
  if (!chatgpt.pending || chatgpt.cancelling) return;
  const seq = ++authSeq;
  chatgpt.cancelling = true;
  accountRevision++;
  invalidateAiModels();
  try { await api.chatgptCancelSignIn(); }
  catch (e) { if (seq === authSeq) chatgpt.error = ApiError.from(e); }
  finally {
    if (seq === authSeq) {
      chatgpt.pending = chatgpt.cancelling = false;
      await loadChatGptStatus();
      if (ai.provider === 'chatgpt') await loadAiModels(true);
    }
  }
}

export async function logoutChatGpt() {
  if (chatgpt.signingOut) return;
  const seq = ++authSeq;
  chatgpt.pending = chatgpt.cancelling = chatgpt.loading = false;
  chatgpt.signingOut = true;
  chatgpt.error = null;
  chatgpt.warning = '';
  chatgpt.welcome = false;
  accountRevision++;
  applyChatGptStatus({ signedIn: false, accountLabel: null, usageEnabled: false });
  invalidateAiModels();
  try {
    const result = await api.chatgptLogout();
    if (seq === authSeq && result.revocationUnconfirmed) chatgpt.warning = 'Signed out locally. Remote revocation was not confirmed; disconnect sandb in ChatGPT Settings if needed.';
  } catch (e) {
    if (seq === authSeq) { chatgpt.error = ApiError.from(e); await loadChatGptStatus(); }
  } finally { if (seq === authSeq) chatgpt.signingOut = false; }
}

export async function manageChatGptUsage() {
  try { await api.chatgptManageUsage(); }
  catch (e) { chatgpt.error = ApiError.from(e); }
}

export function dismissChatGptWelcome() { chatgpt.welcome = false; }
export function resumeChatGptUsage() { chatgpt.usagePaused = false; }

export function chatGptIssue(error: AppErrorBody): { text: string; action?: 'usage' | 'signin' | 'settings' | 'retry'; diagnostic?: string; requestId?: string } {
  const code = error.upstream_code;
  let text = error.message;
  let action: 'usage' | 'signin' | 'settings' | 'retry' | undefined;
  if (code === 'subscription_sharing_usage_limit_exceeded') {
    text = "ChatGPT usage limit reached. Review your plan or this app's limit in ChatGPT settings."; action = 'usage';
  } else if (code === 'subscription_sharing_user_not_eligible') {
    text = 'ChatGPT plan usage is unavailable for this account, workspace, or policy.'; action = 'settings';
  } else if (code === 'subscription_sharing_unsupported_capability') {
    text = 'This capability is not supported for ChatGPT plan usage. Review the model and request settings.'; action = 'settings';
  } else if (code === 'chatpass_v2_scope_not_authorized' || code === 'chatpass_v2_invalid_authorization_context' || code === 'subscription_sharing_route_not_supported') {
    text = 'ChatGPT rejected the permission context or route. Review the integration; repeating sign-in may not fix it.'; action = 'settings';
  } else if (error.http_status === 503 || code === 'subscription_sharing_usage_unavailable' || code === 'subscription_sharing_user_unavailable' || error.code === 'connect_failed' || error.code === 'timeout') {
    text = 'ChatGPT is temporarily unavailable. Your sign-in is preserved; try again later.'; action = 'retry';
  } else if (error.http_status === 403) {
    text = 'ChatGPT plan usage is restricted by policy, permission, or region.'; action = 'settings';
  } else if (error.code === 'auth_failed' || error.http_status === 401) {
    text = 'ChatGPT authorization was not accepted. Continue with ChatGPT again or review plan permission.'; action = 'signin';
  }
  return { text, action, diagnostic: [code, error.upstream_param].filter(Boolean).join(' · ').slice(0, 240) || undefined, requestId: error.request_id?.slice(0, 200) };
}

export function reportChatGptError(error: AppErrorBody) {
  if (error.upstream_code === 'subscription_sharing_usage_limit_exceeded') chatgpt.usagePaused = true;
  if (error.code === 'auth_failed') void loadChatGptStatus();
  return chatGptIssue(error);
}

export function canUseChatGpt() {
  return chatgpt.supported && chatgpt.signedIn && chatgpt.usageEnabled && !chatgpt.pending && !chatgpt.loading && !chatgpt.signingOut && !chatgpt.usagePaused && aiModels.list.includes(ai.model);
}

function modelTarget() {
  return ai.provider === 'chatgpt' ? `chatgpt\n${accountRevision}`
    : [ai.provider, ai.baseUrl.trim().replace(/\/+$/, ''), ai.apiKey.trim() || (ai.hasKey ? '(saved)' : '')].join('\n');
}

export async function loadAiModels(force = false) {
  if (ai.provider === 'chatgpt' && (!chatgpt.supported || !chatgpt.signedIn || !chatgpt.usageEnabled || chatgpt.pending || chatgpt.signingOut || chatgpt.loading)) {
    invalidateAiModels();
    return;
  }
  const target = modelTarget();
  if (!force && aiModels.target === target && (aiModels.loading || (aiModels.list.length && !aiModels.error))) return;
  const seq = ++modelsSeq;
  if (aiModels.target !== target) { aiModels.list = []; aiModels.catalog = []; }
  aiModels.target = target;
  aiModels.loading = true;
  aiModels.error = '';
  try {
    if (ai.provider === 'chatgpt') {
      const catalog = await api.chatgptModels();
      if (seq !== modelsSeq || target !== modelTarget()) return;
      chatgpt.error = null;
      aiModels.catalog = catalog;
      aiModels.list = catalog.map((model) => model.slug);
      if (!aiModels.list.includes(ai.model)) setAiModel(aiModels.list[0] ?? '');
    } else {
      const list = await api.listAiModels(aiSettings(), ai.apiKey.trim() || null);
      if (seq === modelsSeq && target === modelTarget()) aiModels.list = list;
    }
  } catch (e) {
    if (seq === modelsSeq && target === modelTarget()) {
      const error = ApiError.from(e);
      aiModels.error = ai.provider === 'chatgpt' ? chatGptIssue(error).text : error.message;
      if (ai.provider === 'chatgpt') chatgpt.error = error;
    }
  } finally {
    if (seq === modelsSeq && target === modelTarget()) aiModels.loading = false;
  }
}

export function modelLabel(slug: string) {
  return ai.provider === 'chatgpt' ? aiModels.catalog.find((model) => model.slug === slug)?.display_name ?? slug : slug;
}

export function pickProvider(provider: Provider) {
  if (provider === ai.provider) return;
  const previous = ai.provider;
  const preset = providers[previous];
  try { localStorage.setItem(`sandb.ai.model.${previous}`, ai.model); } catch { /* Preferences are optional. */ }
  let remembered: string | null = null;
  try { remembered = localStorage.getItem(`sandb.ai.model.${provider}`); } catch { /* Preferences are optional. */ }
  ai.provider = provider;
  const presetUrl = previous === 'chatgpt'
    ? ai.baseUrl === providers.openai.url || ai.baseUrl === providers.anthropic.url
    : ai.baseUrl === preset.url;
  if (provider !== 'chatgpt' && (!ai.baseUrl.trim() || presetUrl)) ai.baseUrl = providers[provider].url;
  ai.model = remembered ?? (provider === 'chatgpt' ? '' : previous === 'chatgpt' || !ai.model.trim() || ai.model === preset.model ? providers[provider].model : ai.model);
  invalidateAiModels();
  saveAiSoon();
  if (provider === 'chatgpt') void loadAiModels();
}

/** Chooses the model for every chat and saves it. */
export function setAiModel(model: string) {
  const slug = model.trim();
  if (ai.provider === 'chatgpt' && slug && !aiModels.list.includes(slug)) return;
  ai.model = slug;
  saveAiSoon();
}

// Stands in while there are no connections, so views that read active() keep working.
const placeholder: Conn = { id: '', name: '', engine: 'pg', version: '', env: 'local', host: '', port: 0, database: '', user: '', ssl: 'prefer', file: '', aiWrite: false, readOnly: false, schema: 'prod' };

export function active(): Conn {
  const id = activeTab()?.connId;
  return db.connections.find((c) => c.id === id) ?? db.connections[0] ?? placeholder;
}

/** False on a read-only connection: no write UI (editor, AI, edit mode) may act on it. */
export function canWrite(conn: Conn | Connection): boolean {
  return !conn.readOnly;
}
