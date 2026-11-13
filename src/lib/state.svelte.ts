import { api, ApiError, type AiSettings, type Provider } from './api';
import { DEFAULT_AI_BASE_URL, DEFAULT_AI_MODEL, DEFAULT_MAX_ROWS, DEFAULT_TIMEOUT_SEC } from './api/generated/constants';

export const ui = $state({
  dark: false,
  chatOpen: true,
  chatWidth: 340,
  page: 'query' as 'query' | 'settings' | 'audit',
  settingsSection: 'ai' as 'ai' | 'safeguards' | 'appearance' | 'updates',
  theme: 'light' as 'light' | 'dark' | 'system',
  /** Sidebar width in px; the title bar uses it too, so the tabs stay in line with the main card. */
  sidebarWidth: 224,
  sidebarOpen: true,
  sidebarView: 'connections' as 'connections' | 'queries',
  /** Editor height as a fraction of the main card; the results grid takes the rest. */
  editorSplit: 0.34,
  resultsOpen: true,
});

/** The AI settings page's copy; the backend type is `Provider` in the wire contract. */
export const providers: Record<Provider, { label: string; url: string; model: string }> = {
  openai: { label: 'OpenAI-compatible', url: 'https://api.openai.com/v1', model: 'gpt-4.1-mini' },
  anthropic: { label: 'Anthropic-compatible', url: 'https://api.anthropic.com/v1', model: 'claude-sonnet-4-5' },
};

/** The defaults the settings page starts from; `get_ai_settings` replaces them once loaded. */
const aiDefaults: AiSettings = {
  provider: 'openai',
  baseUrl: DEFAULT_AI_BASE_URL,
  model: DEFAULT_AI_MODEL,
  privacy: 'results',
  maxRows: DEFAULT_MAX_ROWS,
  timeoutSec: DEFAULT_TIMEOUT_SEC,
};

export const ai = $state({
  ...aiDefaults,
  /** What the user typed. The stored key never comes back to the UI. */
  apiKey: '',
  /** A key is stored. */
  hasKey: false,
  /** The last debounced save failed; Settings shows it next to the AI fields. Empty when the last save worked. */
  saveError: '',
});

export async function loadAiSettings() {
  try {
    Object.assign(ai, await api.getAiSettings());
  } catch (e) {
    console.warn('AI settings were not loaded; using the defaults.', ApiError.from(e).message);
  }
}

/** Every AI setting the backend stores; the key and the save error are not settings. */
export const aiSettings = (): AiSettings => {
  const { apiKey, hasKey, saveError, ...settings } = ai;
  return settings;
};

// Edits are saved shortly after the user stops typing. One queue keeps the writes in order.
const AI_SAVE_MS = 500;
let aiQueue: Promise<unknown> = Promise.resolve();

function enqueue<T>(f: () => Promise<T>): Promise<T> {
  const p = aiQueue.then(f);
  aiQueue = p.catch(() => {});
  return p;
}

/** One debounced writer: it stays dirty until `write` ran, and clears the save error with it. */
function debounced(write: () => Promise<void>) {
  let dirty = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const flush = async () => {
    clearTimeout(timer);
    if (!dirty) return;
    dirty = false;
    await write();
    ai.saveError = '';
  };
  return {
    soon() {
      dirty = true;
      clearTimeout(timer);
      timer = setTimeout(() => enqueue(flush).catch(saveFailed), AI_SAVE_MS);
    },
    flush,
    drop() {
      dirty = false;
      clearTimeout(timer);
    },
  };
}

const settingsSaver = debounced(async () => {
  ai.hasKey = (await api.saveAiSettings({ settings: aiSettings() })).hasKey;
});

// An empty input keeps the stored key; only removeAiKey deletes it.
const keySaver = debounced(async () => {
  const key = ai.apiKey.trim();
  if (!key) return;
  await api.setAiKey({ key });
  ai.hasKey = true;
});

function saveFailed(e: unknown) {
  ai.saveError = ApiError.from(e).message;
}

export function saveAiSoon() {
  settingsSaver.soon();
}

export function saveAiKeySoon() {
  keySaver.soon();
}

/** Writes pending settings and key now. Fails with the backend error, for example `invalid_input`. */
export function flushAi(): Promise<void> {
  return enqueue(async () => {
    await settingsSaver.flush();
    await keySaver.flush();
  });
}

export async function removeAiKey() {
  keySaver.drop();
  ai.apiKey = '';
  await enqueue(() => api.setAiKey({ key: null }));
  ai.hasKey = false;
  ai.saveError = '';
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
  aiModels.error = '';
  if (!api.capabilities.ai) {
    aiModels.error = 'The demo build has no AI provider.';
    aiModels.loading = false;
    return;
  }
  aiModels.loading = true;
  try {
    const list = await api.listAiModels({ settings: aiSettings(), key: ai.apiKey.trim() || null });
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
