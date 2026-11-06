import assert from 'node:assert/strict';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const calls = [];
const handlers = new Map();
const storage = new Map();
globalThis.localStorage = {
  getItem: (key) => storage.get(key) ?? null,
  setItem: (key, value) => storage.set(key, String(value)),
  removeItem: (key) => storage.delete(key),
};
globalThis.window = {
  __TAURI_INTERNALS__: {
    invoke: async (command, args) => {
      calls.push({ command, args });
      assert(handlers.has(command), `unexpected native command ${command}`);
      return handlers.get(command)(args);
    },
  },
};
const saved = {
  provider: 'chatgpt', baseUrl: 'https://saved-api.example/v1', model: 'account-model',
  privacy: 'schema', maxRows: 500, timeoutSec: 15, hasKey: true,
};
handlers.set('get_ai_settings', () => saved);
const enabled = { signedIn: true, accountLabel: 'dummy@example.test', usageEnabled: true };
const disabled = { ...enabled, usageEnabled: false };
const signedOut = { signedIn: false, accountLabel: null, usageEnabled: false };
let nativeStatus = enabled;
handlers.set('chatgpt_status', () => nativeStatus);
handlers.set('chatgpt_models', () => [
  { slug: 'z-model', display_name: 'First account model', visibility: 'list' },
  { slug: 'a-model', display_name: 'Second account model', visibility: 'list' },
]);
handlers.set('save_ai_settings', ({ settings }) => ({ ...settings, hasKey: true }));

const server = await createServer({ configFile: false, plugins: [svelte()], server: { middlewareMode: true, hmr: false, watch: null } });
try {
  const { render } = await server.ssrLoadModule('svelte/server');
  const state = await server.ssrLoadModule('/src/lib/state.svelte.ts');
  await state.loadAiSettings();
  assert.equal(state.ai.provider, 'chatgpt', 'saved ChatGPT provider must not become an API-key provider');
  assert.equal(state.ai.baseUrl, saved.baseUrl);
  assert.equal(state.ai.apiKey, '', 'stored keys must never be returned to frontend state');
  await state.loadAiModels(true);
  assert.deepEqual(state.aiModels.list, ['z-model', 'a-model'], 'account catalog must retain server order');
  assert.equal(state.modelLabel('z-model'), 'First account model');
  assert.equal(state.ai.model, 'z-model', 'a missing account model must be replaced by a listed slug');
  state.setAiModel('not-in-the-account-catalog');
  assert.equal(state.ai.model, 'z-model', 'ChatGPT picker cannot submit arbitrary model IDs');
  for (const [source, target] of [['anthropic', 'openai'], ['openai', 'anthropic']]) {
    state.pickProvider(source);
    state.ai.baseUrl = state.providers[source].url;
    state.ai.apiKey = 'dummy-switch-key';
    state.pickProvider('chatgpt');
    assert.equal(state.ai.baseUrl, state.providers[source].url, 'entering ChatGPT must retain the saved API URL');
    state.pickProvider(target);
    assert.equal(state.ai.baseUrl, state.providers[target].url, `${source} → ChatGPT → ${target} must select the matching API preset`);
    assert.equal(state.ai.apiKey, 'dummy-switch-key');
  }
  for (const provider of ['openai', 'anthropic']) {
    state.pickProvider(provider);
    state.ai.baseUrl = 'https://custom-api.example/v1';
    state.ai.model = `custom-${provider}-model`;
    state.pickProvider('chatgpt');
    state.pickProvider(provider);
    assert.equal(state.ai.baseUrl, 'https://custom-api.example/v1');
    assert.equal(state.ai.model, `custom-${provider}-model`);
    assert.equal(state.ai.apiKey, 'dummy-switch-key');
    state.pickProvider('chatgpt');
    state.pickProvider(provider === 'openai' ? 'anthropic' : 'openai');
    assert.equal(state.ai.baseUrl, 'https://custom-api.example/v1', 'custom URLs must survive a change of API format too');
    assert.equal(state.ai.apiKey, 'dummy-switch-key');
  }
  state.ai.apiKey = '';
  await state.loadAiSettings();
  const SettingsPage = (await server.ssrLoadModule('/src/lib/components/SettingsPage.svelte')).default;
  const ModelPicker = (await server.ssrLoadModule('/src/lib/components/ModelPicker.svelte')).default;
  let markup = render(SettingsPage, { props: { onEnableWrites() {} } }).body;
  assert(markup.includes('Switch account'), 'signed-in accounts offer an account switch');
  assert(!markup.includes('Continue with ChatGPT'), 'sign-in branding is only shown while signed out');
  assert(markup.includes('Connected'));
  assert(markup.includes('About ChatGPT plan'));
  assert(markup.includes('dummy@example.test'));
  assert(!markup.includes('type="password"'), 'API-key input must be hidden for ChatGPT');
  assert(!markup.includes('data-oai-siwc-button'), 'official sign-in button is reserved for signed-out state');
  assert(markup.indexOf('Account model') < markup.indexOf('Switch account'), 'model sits above the account actions');
  assert(render(ModelPicker, { props: { variant: 'field' } }).body.includes('First account model'));
  state.chatgpt.loading = true;
  markup = render(SettingsPage, { props: { onEnableWrites() {} } }).body;
  assert(markup.includes('Checking ChatGPT sign-in'));
  assert(!markup.includes('Continue with ChatGPT'));
  state.chatgpt.loading = false;
  state.chatgpt.supported = false;
  markup = render(SettingsPage, { props: { onEnableWrites() {} } }).body;
  assert(markup.includes('require the native desktop app'));
  state.chatgpt.supported = true;
  const catalog = handlers.get('chatgpt_models');
  const deferred = () => {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
  };
  let old = deferred();
  handlers.set('chatgpt_models', () => old.promise);
  const stale = state.loadAiModels(true);
  state.pickProvider('openai');
  state.ai.model = 'my-api-model';
  state.ai.apiKey = 'dummy-typed-api-key';
  handlers.set('list_ai_models', () => ['my-api-model']);
  await state.loadAiModels(true);
  old.resolve([{ slug: 'stale-account', display_name: 'Wrong account', visibility: 'list' }]);
  await stale;
  assert.deepEqual(state.aiModels.list, ['my-api-model'], 'late account catalogs must not overwrite another provider');
  handlers.set('chatgpt_models', catalog);
  state.pickProvider('chatgpt');
  await state.loadAiModels(true);
  state.pickProvider('openai');
  assert.equal(state.ai.model, 'my-api-model');
  assert.equal(state.ai.baseUrl, saved.baseUrl);
  assert.equal(state.ai.apiKey, 'dummy-typed-api-key');
  state.pickProvider('chatgpt');
  await state.loadAiModels(true);
  handlers.set('chatgpt_sign_in', ({ enableUsage }) => {
    assert.equal(typeof enableUsage, 'boolean');
    nativeStatus = enableUsage ? enabled : disabled;
    return { ...nativeStatus, access_token: 'must-not-be-copied', authorization_url: 'must-not-be-copied' };
  });
  await state.signInChatGpt();
  assert.equal(state.chatgpt.signedIn, true);
  assert.equal(state.chatgpt.usageEnabled, false);
  assert.equal(state.canUseChatGpt(), false);
  assert.deepEqual(state.aiModels.list, []);
  markup = render(SettingsPage, { props: { onEnableWrites() {} } }).body;
  assert(markup.includes('Enable ChatGPT plan usage'));
  assert(markup.includes('Plan usage is not enabled'));
  assert(!markup.includes('Continue with ChatGPT'));
  await state.signInChatGpt(true);
  assert.equal(state.canUseChatGpt(), true);
  assert.equal(state.chatgpt.welcome, true);
  assert.equal(storage.get('sandb.chatgpt.welcomed'), 'yes');
  assert.equal(state.chatgpt.access_token, undefined);
  assert.equal(state.chatgpt.authorization_url, undefined);
  state.dismissChatGptWelcome();
  await state.signInChatGpt(true);
  assert.equal(state.chatgpt.welcome, false, 'welcome must not repeat on reauthorization');
  assert.deepEqual(calls.filter((call) => call.command === 'chatgpt_sign_in').map((call) => call.args), [{ enableUsage: false }, { enableUsage: true }, { enableUsage: true }]);
  handlers.set('chatgpt_sign_in', () => Promise.reject({ code: 'auth_failed', message: 'dummy authorization declined' }));
  await state.signInChatGpt();
  assert.equal(state.canUseChatGpt(), true, 'failed reauthorization must leave the old session usable');
  assert.equal(state.chatgpt.error.message, 'dummy authorization declined');
  const pending = deferred();
  handlers.set('chatgpt_sign_in', () => pending.promise);
  handlers.set('chatgpt_cancel_sign_in', () => { pending.reject({ code: 'cancelled', message: 'Stopped' }); });
  const login = state.signInChatGpt();
  assert.equal(state.chatgpt.pending, true);
  markup = render(SettingsPage, { props: { onEnableWrites() {} } }).body;
  assert(markup.includes('Cancel sign-in'));
  assert(markup.includes('Complete sign-in in your system browser'));
  assert(!markup.includes('Switch account'));
  assert(!markup.includes('Sign out'));
  assert(!markup.includes('Continue with ChatGPT'));
  await state.cancelChatGptSignIn();
  await login;
  assert.equal(state.chatgpt.pending, false);
  assert.equal(state.chatgpt.signedIn, true, 'cancel preserves the previously active session');
  assert.equal(state.chatgpt.error, null);
  old = deferred();
  handlers.set('chatgpt_models', () => old.promise);
  const beforeLogout = state.loadAiModels(true);
  handlers.set('chatgpt_logout', () => { nativeStatus = signedOut; return { revocationUnconfirmed: true }; });
  await state.logoutChatGpt();
  old.resolve([{ slug: 'stale-after-logout', display_name: 'Old catalog', visibility: 'list' }]);
  await beforeLogout;
  assert.deepEqual(state.aiModels.list, []);
  assert.equal(state.canUseChatGpt(), false);
  assert.match(state.chatgpt.warning, /not confirmed/);
  markup = render(SettingsPage, { props: { onEnableWrites() {} } }).body;
  assert(markup.includes('Continue with ChatGPT'));
  assert(markup.includes('data-oai-siwc-button'));
  assert(!markup.includes('Switch account'));
  assert(!markup.includes('Account model'));
  assert(markup.includes('Remote revocation was not confirmed'));
  const lateLogin = deferred();
  handlers.set('chatgpt_sign_in', () => lateLogin.promise);
  const late = state.signInChatGpt();
  await state.logoutChatGpt();
  lateLogin.resolve(enabled);
  await late;
  assert.equal(state.chatgpt.signedIn, false, 'a stale login result must not resurrect a signed-out UI');
  handlers.set('chatgpt_manage_usage', () => {});
  await state.manageChatGptUsage();
  assert.deepEqual(calls.at(-1), { command: 'chatgpt_manage_usage', args: {} });
  assert.equal(state.chatGptIssue({ code: 'ai_upstream', message: 'quota', upstream_code: 'subscription_sharing_usage_limit_exceeded' }).action, 'usage');
  assert.equal(state.chatGptIssue({ code: 'auth_failed', message: 'ineligible', http_status: 403, upstream_code: 'subscription_sharing_user_not_eligible' }).action, 'settings');
  assert.equal(state.chatGptIssue({ code: 'auth_failed', message: 'context', upstream_code: 'chatpass_v2_invalid_authorization_context' }).action, 'settings');
  assert.equal(state.chatGptIssue({ code: 'ai_upstream', message: 'temporary', http_status: 503 }).action, 'retry');
  state.reportChatGptError({ code: 'ai_upstream', message: 'quota', upstream_code: 'subscription_sharing_usage_limit_exceeded' });
  assert.equal(state.chatgpt.usagePaused, true);
  state.resumeChatGptUsage();
  assert.equal(state.chatgpt.usagePaused, false);
  nativeStatus = enabled;
  handlers.set('chatgpt_models', catalog);
  await state.loadChatGptStatus();
  handlers.set('chatgpt_models', () => Promise.reject({ code: 'ai_upstream', message: 'temporarily unavailable', http_status: 503, request_id: 'req-safe', upstream_body: '<script>untrusted-body</script>' }));
  await state.loadAiModels(true);
  markup = render(SettingsPage, { props: { onEnableWrites() {} } }).body;
  assert(markup.includes('temporarily unavailable'));
  assert(markup.includes('req-safe'));
  assert(!markup.includes('untrusted-body'), 'UI must not dump wholesale upstream bodies');
  handlers.set('chatgpt_models', catalog);
  await state.loadAiModels(true);
  assert.equal(state.chatgpt.error, null, 'successful catalog recovery clears stale errors');
  const originalFetch = globalThis.fetch;
  const rpc = [];
  globalThis.fetch = async (url, options) => {
    rpc.push({ command: String(url).split('/').at(-1), args: JSON.parse(options.body) });
    return String(url).endsWith('/chatgpt_status')
      ? Response.json(signedOut)
      : Response.json({ code: 'invalid_input', message: 'ChatGPT requires the native desktop app' }, { status: 400 });
  };
  try {
    const { httpBackend } = await server.ssrLoadModule('/src/lib/api/http.ts');
    assert.deepEqual(await httpBackend.chatgptStatus(), signedOut);
    await assert.rejects(httpBackend.chatgptSignIn(true), /native desktop app/);
    assert.deepEqual(rpc.at(-1), { command: 'chatgpt_sign_in', args: { enableUsage: true } });
  } finally { globalThis.fetch = originalFetch; }
  const { mockBackend } = await server.ssrLoadModule('/src/lib/api/mock.ts');
  assert.deepEqual(await mockBackend.chatgptStatus(), signedOut);
  await assert.rejects(mockBackend.chatgptSignIn(false), /native desktop app/);
  await assert.rejects(mockBackend.chatgptLogout(), /native desktop app/);
  assert(!JSON.stringify([...storage]).includes('dummy-typed-api-key'), 'only nonsecret preferences may enter browser storage');
  await state.flushAi();
  console.log('ChatGPT frontend self-check passed');
} finally {
  await server.close();
  delete globalThis.window;
  delete globalThis.localStorage;
}
