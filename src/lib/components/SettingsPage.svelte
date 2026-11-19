<script lang="ts">
  import { Bot, KeyRound, Eye, EyeOff, Lock, Gauge, ShieldCheck, Plug, Copy, Terminal, Share2, Palette, Sun, Moon, Monitor, Check, LoaderCircle, X, CircleArrowUp, Image, AppWindow, Square, SlidersHorizontal } from '@lucide/svelte';
  import { db, envTag, canWrite, setAiWrite, type Conn } from '../connections.svelte';
  import { ui, ai, providers, aiSettings, saveAiSoon, saveAiKeySoon, flushAi, removeAiKey } from '../state.svelte';
  import type { McpSettings, McpStatus, Provider } from '../api';
  import { Flash } from '../flash.svelte';
  import { MAX_ROWS_LIMIT, TIMEOUT_SEC_LIMIT } from '../api/generated/constants';
  import { look, canSeeThrough, backdropOn, lookChanged, setTheme, setBackgroundImage, removeBackgroundImage, resetBackdrop, type Backdrop } from '../appearance.svelte';
  import { api, ApiError } from '../api';
  import { appVersion, updates, checkForUpdates, installUpdate, restartApp, updatesEnabled as canUpdate } from '../updates.svelte';
  import ModelPicker from './ModelPicker.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import EngineBadge from './ui/EngineBadge.svelte';
  import PageHeader from './ui/PageHeader.svelte';
  import Switch from './ui/Switch.svelte';

  const apiKeyId = $props.id();
  let { onEnableWrites }: { onEnableWrites: (c: Conn) => void } = $props();

  let showKey = $state(false);
  let test = $state<'idle' | 'testing' | 'ok' | 'fail'>('idle');
  let tested = $state<{ models: number; ms: number } | null>(null);
  let testError = $state<ApiError | null>(null);
  // A test that a later edit made out of date must not show its result.
  let testSeq = 0;

  const keyNote = api.capabilities.secretStorage === 'encrypted' ? 'Stored encrypted on this computer.' : api.capabilities.secretStorage === 'memory' ? 'Kept in memory while the dev bridge runs.' : 'Demo build: no AI backend.';

  function changed() {
    testSeq++;
    test = 'idle';
    saveAiSoon();
  }

  function keyChanged() {
    testSeq++;
    test = 'idle';
    saveAiKeySoon();
  }

  // Switching keeps a URL and model the user typed; only empty fields or the other preset's values change.
  function pick(id: Provider) {
    const other = providers[ai.provider];
    ai.provider = id;
    if (!ai.baseUrl.trim() || ai.baseUrl === other.url) ai.baseUrl = providers[id].url;
    if (!ai.model.trim() || ai.model === other.model) ai.model = providers[id].model;
    changed();
  }

  async function removeKey() {
    testSeq++;
    test = 'idle';
    try {
      await removeAiKey();
    } catch (e) {
      testError = ApiError.from(e);
      test = 'fail';
    }
  }

  async function runTest() {
    const seq = ++testSeq;
    test = 'testing';
    try {
      await flushAi();
      const r = await api.testAi({ settings: aiSettings(), key: ai.apiKey || null });
      if (seq !== testSeq) return;
      tested = { models: r.modelCount, ms: r.latencyMs };
      test = 'ok';
    } catch (e) {
      if (seq !== testSeq) return;
      testError = ApiError.from(e);
      test = 'fail';
    }
  }

  function toggleWrites(c: Conn, enabled: boolean) {
    if (!enabled) void setAiWrite(c, false);
    else if (canWrite(c)) onEnableWrites(c);
  }

  // MCP: the server and its token come from the backend; the section loads them when it opens.
  let mcp = $state<McpStatus | null>(null);
  let mcpError = $state<ApiError | null>(null);
  let showToken = $state(false);
  const copied = new Flash<string>();

  $effect(() => {
    if (ui.settingsSection !== 'mcp' || mcp !== null) return;
    void loadMcp();
  });

  async function loadMcp() {
    try {
      mcp = await api.getMcpStatus();
      mcpError = null;
    } catch (e) {
      mcpError = ApiError.from(e);
    }
  }

  async function saveMcp(patch: Partial<McpSettings>) {
    if (!mcp) return;
    try {
      mcp = await api.saveMcpSettings({ enabled: mcp.enabled, port: mcp.port, connections: mcp.connections, ...patch });
      mcpError = null;
    } catch (e) {
      mcpError = ApiError.from(e);
    }
  }

  function share(id: string, on: boolean) {
    const current = mcp;
    if (!current) return;
    // The saved order is the page order, so a client's `list_connections` matches what the user sees.
    const connections = db.connections.map((c) => c.id).filter((cid) => (cid === id ? on : current.connections.includes(cid)));
    void saveMcp({ connections });
  }

  async function newToken() {
    try {
      mcp = await api.newMcpToken();
      mcpError = null;
    } catch (e) {
      mcpError = ApiError.from(e);
    }
  }

  async function copyMcp(key: string, text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied.show(key);
    } catch (err) {
      console.warn('Copy failed.', err);
    }
  }

  const claudeCommand = $derived(mcp ? `claude mcp add --transport http sandb ${mcp.url} --header "Authorization: Bearer ${mcp.token}"` : '');
  const clientJson = $derived(mcp ? JSON.stringify({ mcpServers: { sandb: { url: mcp.url, headers: { Authorization: `Bearer ${mcp.token}` } } } }, null, 2) : '');

  const sections = [
    { id: 'ai', l: 'AI', i: Bot },
    { id: 'safeguards', l: 'Safeguards', i: ShieldCheck },
    // The server runs in the desktop app only.
    ...(api.capabilities.mcp ? [{ id: 'mcp' as const, l: 'MCP', i: Plug }] : []),
    { id: 'appearance', l: 'Appearance', i: Palette },
    { id: 'updates', l: 'Updates', i: CircleArrowUp },
  ] as const;

  const backdrops = [
    { id: 'none', l: 'None', i: Square },
    { id: 'image', l: 'Image', i: Image },
    ...(canSeeThrough ? [{ id: 'desktop', l: 'See-through', i: AppWindow }] : []),
  ] as { id: Backdrop; l: string; i: typeof Square }[];

  let imageInput = $state<HTMLInputElement>();
  let imageBusy = $state(false);
  let imageError = $state('');

  function pickBackdrop(id: Backdrop) {
    look.backdrop = id;
    lookChanged();
    // With no image yet, "Image" goes straight to the file picker.
    if (id === 'image' && !look.image) imageInput?.click();
  }

  async function imagePicked(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    imageBusy = true;
    imageError = '';
    try {
      await setBackgroundImage(file);
    } catch (err) {
      imageError = err instanceof Error ? err.message : String(err);
    } finally {
      imageBusy = false;
    }
  }
</script>

{#snippet slider(label: string, key: 'tint' | 'panels' | 'imageBlur' | 'panelBlur', min: number, max: number, unit: string, hint: string)}
  <label class="block">
    <span class="label mb-1 flex items-center">{label}<span class="ml-auto font-mono text-xs text-faint tabular-nums">{look[key]}{unit}</span></span>
    <input class="focus-ring h-5 w-full cursor-pointer" type="range" {min} {max} bind:value={look[key]} oninput={lookChanged} />
    <span class="mt-0.5 block text-sm text-faint">{hint}</span>
  </label>
{/snippet}

<div class="flex h-full flex-col">
  <PageHeader title="Settings" subtitle="AI provider, privacy, safeguards, MCP and appearance" onback={() => (ui.page = 'query')} />

  <nav class="flex shrink-0 items-end gap-1 overflow-x-auto border-b border-line px-4" aria-label="Settings sections">
    {#each sections as s (s.id)}
      <button
        class="focus-ring -mb-px flex h-sub-toolbar shrink-0 items-center gap-1.5 border-b-2 px-2 text-sm font-medium focus-visible:-outline-offset-2 motion-safe:transition-colors motion-safe:duration-150 {ui.settingsSection === s.id ? 'border-accent text-ink' : 'border-transparent text-muted hover:text-ink'}"
        aria-current={ui.settingsSection === s.id ? 'page' : undefined}
        onclick={() => (ui.settingsSection = s.id)}><s.i size={13} /> {s.l}</button>
    {/each}
  </nav>

  <div class="min-h-0 flex-1 overflow-auto">
    <div class="mx-auto w-full max-w-3xl divide-y divide-line px-6 py-2">
      {#if ui.settingsSection === 'ai'}
      <!-- AI provider -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><Bot size={13} class="text-faint" /> AI provider</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">Any OpenAI-compatible or Anthropic-compatible endpoint. Your key stays on this computer.</p>
        </div>
        <div class="min-w-0">
          <div class="seg flex-wrap">
            {#each Object.entries(providers) as [id, v] (id)}
              <button class="seg-item {ai.provider === id ? 'seg-on' : ''}" aria-pressed={ai.provider === id} onclick={() => pick(id as Provider)}>{v.label}</button>
            {/each}
          </div>

          <div class="mt-3 grid gap-3 sm:grid-cols-2">
            <label class="block sm:col-span-2">
              <span class="label mb-1.5 block">Base URL</span>
              <input class="input font-mono text-sm" bind:value={ai.baseUrl} oninput={changed} placeholder={providers[ai.provider].url} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
            </label>
            <div class="block">
              <div class="label mb-1.5 flex min-w-0 items-center gap-1 whitespace-nowrap">
                <label for={apiKeyId} class="flex min-w-0 items-center gap-1"><span class="shrink-0">API key</span> <span class="min-w-0 truncate font-normal text-faint" title="Optional for local servers">(optional for local servers)</span></label>
                {#if ai.hasKey}<button class="focus-ring ml-auto shrink-0 cursor-pointer rounded px-1 font-medium text-muted hover:bg-danger-soft hover:text-danger" onclick={removeKey} title="Delete the saved key">Remove</button>{/if}
              </div>
              <div class="relative">
                <input id={apiKeyId} class="input pr-9 font-mono text-sm" type={showKey ? 'text' : 'password'} bind:value={ai.apiKey} oninput={keyChanged} placeholder={ai.hasKey ? '•••••••• (saved)' : ai.provider === 'anthropic' ? 'sk-ant-…' : 'sk-…'} title={ai.hasKey ? 'Saved key is hidden. Type to replace it.' : undefined} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
                <button class="focus-ring absolute inset-y-0 right-1 grid w-7 cursor-pointer place-items-center rounded text-faint hover:text-ink" onclick={() => (showKey = !showKey)} aria-label={showKey ? 'Hide key' : 'Show key'}>
                  {#if showKey}<EyeOff size={13} />{:else}<Eye size={13} />{/if}
                </button>
              </div>
            </div>
            <div class="block">
              <span class="label mb-1.5 block">Model</span>
              <ModelPicker variant="field" placeholder={providers[ai.provider].model} onpick={changed} />
            </div>
          </div>

          <div class="mt-3 flex items-center gap-3">
            <span class="flex items-center gap-1.5 text-xs text-faint"><KeyRound size={12} /> {keyNote}</span>
            <div class="flex-1"></div>
            {#if test === 'ok' && tested}<span class="flex items-center gap-1 text-sm font-medium text-ok"><Check size={13} /> Connected · {tested.models.toLocaleString()} {tested.models === 1 ? 'model' : 'models'} · {tested.ms} ms</span>
            {:else if test === 'fail'}<span class="flex items-center gap-1 text-sm font-medium text-danger"><X size={13} /> Failed</span>
            {:else if test === 'testing'}<span class="text-sm text-muted">testing…</span>{/if}
            {#if api.capabilities.ai}
              <button class="btn btn-secondary" onclick={runTest} disabled={test === 'testing'}>
                {#if test === 'testing'}<LoaderCircle size={13} class="motion-safe:animate-spin" />{/if} Test
              </button>
            {/if}
          </div>
          {#if test === 'fail' && testError}
            <p class="mt-1.5 text-right text-sm break-words text-muted">{testError.message}{#if testError.code === 'model_not_found' && testError.detail}{' '}<span class="text-faint">Models: {testError.detail}</span>{/if}</p>
          {/if}
          {#if ai.saveError}
            <p class="mt-1.5 text-right text-sm break-words text-danger" role="alert">These settings were not saved: {ai.saveError}</p>
          {/if}
        </div>
      </section>

      <!-- Privacy -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><Lock size={13} class="text-faint" /> What the AI can see</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">Choose what leaves this machine when you ask the assistant.</p>
        </div>
        <div class="grid min-w-0 gap-2 sm:grid-cols-2">
          {#each [{ id: 'results', t: 'Schema and query results', d: 'Best answers. Result rows go to the model.' }, { id: 'schema', t: 'Schema only', d: 'Rows never leave this computer. The AI only writes SQL.' }] as o (o.id)}
            <button class="focus-ring cursor-pointer rounded-lg border px-3.5 py-3 text-left motion-safe:transition-colors motion-safe:duration-150 {ai.privacy === o.id ? 'border-accent/70 bg-accent/6 ring-3 ring-accent/12' : 'border-line hover:bg-hover'}" aria-pressed={ai.privacy === o.id} onclick={() => { ai.privacy = o.id as 'results' | 'schema'; changed(); }}>
              <div class="font-medium">{o.t}</div>
              <div class="mt-0.5 text-sm leading-snug text-muted">{o.d}</div>
            </button>
          {/each}
        </div>
      </section>

      <!-- Limits -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><Gauge size={13} class="text-faint" /> Limits for AI queries</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">Caps applied to every query the assistant runs.</p>
        </div>
        <div class="grid min-w-0 gap-3 sm:grid-cols-2">
          <label class="block">
            <span class="label mb-1.5 block">Max rows</span>
            <input class="input font-mono tabular-nums" type="number" min="1" max={MAX_ROWS_LIMIT} bind:value={ai.maxRows} oninput={changed} />
          </label>
          <label class="block">
            <span class="label mb-1.5 block">Timeout (seconds)</span>
            <input class="input font-mono tabular-nums" type="number" min="1" max={TIMEOUT_SEC_LIMIT} bind:value={ai.timeoutSec} oninput={changed} />
          </label>
        </div>
      </section>

      {:else if ui.settingsSection === 'safeguards'}
      <!-- Safeguards -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><ShieldCheck size={13} class="text-faint" /> Safeguards</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">AI writes are off for every connection by default. With write mode on, each write still needs your approval.</p>
        </div>
        <div class="min-w-0 divide-y divide-line rounded-lg border border-line">
          <!-- SQL and MongoDB connections: the chat can propose writes on both, each still needs approval. -->
          {#each db.connections as c (c.id)}
            {@const on = c.aiWrite && canWrite(c)}
            <div class="flex items-center gap-2 px-3 py-2.5">
              <EngineBadge engine={c.engine} class="w-5" />
              <span class="truncate font-medium">{c.name}</span>
              <span class="tag {envTag[c.env]}">{c.env}</span>
              <div class="flex-1"></div>
              {#if canWrite(c)}
                <span class="text-sm {on ? 'text-warn' : 'text-muted'}">{on ? 'Writes need approval' : 'Writes off'}</span>
              {:else}
                <span class="text-sm text-muted">Connection is read-only</span>
              {/if}
              <Switch
                checked={on}
                tone="warn"
                aria-label="AI write mode for {c.name}"
                disabled={!canWrite(c)}
                title={canWrite(c) ? undefined : 'Turn off read-only in the connection settings to allow AI writes'}
                onchange={(enabled) => toggleWrites(c, enabled)} />
            </div>
          {:else}
            <EmptyState title="No connections" hint="Add a connection to manage AI write access." />
          {/each}
        </div>
      </section>

      {:else if ui.settingsSection === 'mcp'}
      {#if !mcp}
        <p class="py-6 text-sm {mcpError ? 'text-danger' : 'text-muted'}">{mcpError?.message ?? 'Loading…'}</p>
      {:else}
      {@const status = mcp}
      <!-- MCP server -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><Plug size={13} class="text-faint" /> MCP server</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">Let AI tools such as Claude Code and Cursor read the connections that you choose. Writes are never allowed.</p>
        </div>
        <div class="min-w-0">
          <div class="flex items-center gap-2.5">
            <Switch checked={status.enabled} aria-label="MCP server" onchange={(enabled) => void saveMcp({ enabled })} />
            <span class="text-sm">{status.enabled ? 'On' : 'Off'}</span>
          </div>
          <label class="mt-3 block max-w-[9rem]">
            <span class="label mb-1.5 block">Port</span>
            <input class="input font-mono tabular-nums" type="number" min="1024" max="65535" value={status.port} onchange={(e) => saveMcp({ port: Number(e.currentTarget.value) })} />
          </label>
          <div class="mt-3 text-sm">
            {#if status.running}
              <span class="flex items-center gap-1 font-medium text-ok"><Check size={13} /> Running at {status.url}</span>
            {:else if status.error}
              <span class="flex items-center gap-1 text-danger"><X size={13} /> {status.error}</span>
            {:else}
              <span class="text-muted">Off</span>
            {/if}
          </div>
          {#if mcpError}
            <p class="mt-1.5 text-sm break-words text-danger" role="alert">{mcpError.message}</p>
          {/if}
        </div>
      </section>

      <!-- Shared connections -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><Share2 size={13} class="text-faint" /> Shared connections</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">Only the connections you tick are visible to MCP clients.</p>
        </div>
        <div class="min-w-0">
          {#if db.connections.length === 0}
            <EmptyState title="No connections" hint="Add a connection before sharing it with MCP clients." />
          {:else}
            <div class="divide-y divide-line rounded-lg border border-line">
              {#each db.connections as c (c.id)}
                <label class="flex items-center gap-2 px-3 py-2.5">
                  <EngineBadge engine={c.engine} class="w-5" />
                  <span class="truncate font-medium">{c.name}</span>
                  <span class="tag {envTag[c.env]}">{c.env}</span>
                  <div class="flex-1"></div>
                  <input
                    class="focus-ring size-4 shrink-0"
                    type="checkbox"
                    checked={status.connections.includes(c.id)}
                    aria-label="Share {c.name} with MCP clients"
                    onchange={(e) => share(c.id, e.currentTarget.checked)} />
                </label>
              {/each}
            </div>
          {/if}
        </div>
      </section>

      <!-- Connect a client -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><Terminal size={13} class="text-faint" /> Connect a client</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">The token is the only credential. Treat it like a password.</p>
        </div>
        <div class="min-w-0">
          <div class="flex min-w-0 items-center gap-2">
            <code class="min-w-0 flex-1 truncate rounded-md border border-line bg-surface px-2.5 py-1.5 font-mono text-sm">{showToken ? status.token : `••••${status.token.slice(-4)}`}</code>
            <button class="btn btn-ghost size-7 shrink-0 p-0" onclick={() => (showToken = !showToken)} aria-label={showToken ? 'Hide token' : 'Show token'}>
              {#if showToken}<EyeOff size={13} />{:else}<Eye size={13} />{/if}
            </button>
            <button class="btn btn-ghost size-7 shrink-0 p-0" onclick={() => copyMcp('token', status.token)} aria-label="Copy token">
              {#if copied.current === 'token'}<Check size={13} class="text-ok" />{:else}<Copy size={13} />{/if}
            </button>
            <button class="btn btn-secondary shrink-0" onclick={newToken} title="Clients that use the old token stop working">New token</button>
          </div>
          <div class="mt-4 space-y-2">
            <div class="relative rounded-lg border border-line bg-surface">
              <pre class="overflow-auto px-3 py-2.5 pr-20 font-mono text-sm leading-relaxed whitespace-pre-wrap">{claudeCommand}</pre>
              <button class="btn btn-sm btn-ghost absolute top-1.5 right-1.5" onclick={() => copyMcp('claude', claudeCommand)}>
                {#if copied.current === 'claude'}<Check size={12} class="text-ok" /> Copied{:else}<Copy size={12} /> Copy{/if}
              </button>
            </div>
            <div class="relative rounded-lg border border-line bg-surface">
              <pre class="overflow-auto px-3 py-2.5 pr-20 font-mono text-sm leading-relaxed whitespace-pre-wrap">{clientJson}</pre>
              <button class="btn btn-sm btn-ghost absolute top-1.5 right-1.5" onclick={() => copyMcp('json', clientJson)}>
                {#if copied.current === 'json'}<Check size={12} class="text-ok" /> Copied{:else}<Copy size={12} /> Copy{/if}
              </button>
            </div>
          </div>
          <p class="mt-2 text-sm text-faint">Claude Desktop: use <span class="font-mono">npx mcp-remote {status.url} --header "Authorization: Bearer …"</span></p>
        </div>
      </section>
      {/if}

      {:else if ui.settingsSection === 'appearance'}
      <!-- Theme -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><Palette size={13} class="text-faint" /> Theme</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">Colors for the whole app.</p>
        </div>
        <div class="min-w-0">
          <div class="seg">
            {#each [{ id: 'light', l: 'Light', i: Sun }, { id: 'dark', l: 'Dark', i: Moon }, { id: 'system', l: 'System', i: Monitor }] as t (t.id)}
              <button
                class="seg-item flex items-center gap-1.5 {ui.theme === t.id ? 'seg-on' : ''}"
                aria-pressed={ui.theme === t.id}
                onclick={() => setTheme(t.id as 'light' | 'dark' | 'system')}><t.i size={13} /> {t.l}</button>
            {/each}
          </div>
        </div>
      </section>

      <!-- Background -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><Image size={13} class="text-faint" /> Background</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">
            Show an image behind the app{#if canSeeThrough}, or make the window see-through{/if}. The image stays on this computer.
          </p>
        </div>
        <div class="min-w-0">
          <div class="seg">
            {#each backdrops as b (b.id)}
              <button class="seg-item flex items-center gap-1.5 {look.backdrop === b.id ? 'seg-on' : ''}" aria-pressed={look.backdrop === b.id} onclick={() => pickBackdrop(b.id)}><b.i size={13} /> {b.l}</button>
            {/each}
          </div>
          <input bind:this={imageInput} class="hidden" type="file" accept="image/*" onchange={imagePicked} />

          {#if look.backdrop === 'image'}
            <div class="mt-3 flex items-center gap-3">
              {#if look.image}
                <div class="h-14 w-24 shrink-0 rounded-md border border-line bg-cover bg-center" style:background-image="url({JSON.stringify(look.image)})" role="img" aria-label="Current background image"></div>
              {:else}
                <div class="grid h-14 w-24 shrink-0 place-items-center rounded-md border border-dashed border-line text-faint"><Image size={15} /></div>
              {/if}
              <button class="btn btn-secondary" onclick={() => imageInput?.click()} disabled={imageBusy}>
                {#if imageBusy}<LoaderCircle size={13} class="motion-safe:animate-spin" />{/if} {look.image ? 'Change image…' : 'Choose image…'}
              </button>
              {#if look.image}<button class="btn btn-ghost enabled:hover:bg-danger-soft enabled:hover:text-danger" onclick={removeBackgroundImage}>Remove</button>{/if}
            </div>
            {#if imageError}<p class="mt-1.5 text-sm text-danger">{imageError}</p>{/if}
          {:else if look.backdrop === 'desktop'}
            <div class="mt-3 flex items-center gap-2.5">
              <Switch checked={look.vibrancy} aria-label="Blur the desktop behind the window" onchange={(checked) => { look.vibrancy = checked; lookChanged(); }} />
              <span class="text-sm">Blur the desktop behind the window</span>
            </div>
          {/if}
        </div>
      </section>

      {#if backdropOn()}
        <!-- Transparency -->
        <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
          <div>
            <h2 class="flex items-center gap-2 text-base font-semibold"><SlidersHorizontal size={13} class="text-faint" /> Transparency</h2>
            <p class="mt-1 text-sm leading-relaxed text-muted">How much of the background shows through. Lower values are harder to read.</p>
          </div>
          <div class="grid min-w-0 gap-4">
            {@render slider('Tint', 'tint', 0, 100, '%', 'Canvas color over the background. The sidebar and title bar sit on it.')}
            {@render slider('Panel opacity', 'panels', 50, 100, '%', 'The editor, results and assistant panels. Below 100% they become frosted glass.')}
            {#if look.backdrop === 'image'}
              {@render slider('Panel blur', 'panelBlur', 0, 100, '%', look.panels < 100 ? 'How much the glass panels blur the image behind them.' : 'Set panel opacity below 100% to see this blur.')}
              {@render slider('Image blur', 'imageBlur', 0, 100, '%', 'Makes the image soft, so text stays clear.')}
            {/if}
            <div><button class="btn btn-ghost -ml-2.5" onclick={resetBackdrop}>Reset background</button></div>
          </div>
        </section>
      {/if}

      {:else}
      <!-- Updates -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-base font-semibold"><CircleArrowUp size={13} class="text-faint" /> Updates</h2>
          <p class="mt-1 text-sm leading-relaxed text-muted">sandb asks before it installs anything.</p>
        </div>
        <div class="min-w-0">
          <div class="flex items-center gap-3">
            <span class="font-mono text-sm">sandb v{appVersion}</span>
            <div class="flex-1"></div>
            {#if updates.status === 'latest'}<span class="flex items-center gap-1 text-sm font-medium text-ok"><Check size={13} /> Up to date</span>
            {:else if updates.status === 'available'}<span class="text-sm font-medium text-warn">v{updates.next} is available</span>
            {:else if updates.status === 'error'}<span class="flex items-center gap-1 text-sm font-medium text-danger"><X size={13} /> Check failed</span>{/if}
            {#if updates.status === 'restart'}
              <span class="text-sm font-medium text-warn">v{updates.next} is installed</span>
              <button class="btn btn-primary" onclick={restartApp}>Restart to finish the update</button>
            {:else if updates.status === 'available' || updates.status === 'installing'}
              <button class="btn btn-primary" onclick={installUpdate} disabled={updates.status === 'installing'}>
                {#if updates.status === 'installing'}<LoaderCircle size={13} class="motion-safe:animate-spin" /> Installing…{:else}Install and restart{/if}
              </button>
            {:else}
              <button class="btn btn-secondary" onclick={() => checkForUpdates()} disabled={!canUpdate || updates.status === 'checking'} title={canUpdate ? undefined : 'Updates work in the installed desktop app only'}>
                {#if updates.status === 'checking'}<LoaderCircle size={13} class="motion-safe:animate-spin" />{/if} Check for updates
              </button>
            {/if}
          </div>
          {#if updates.status === 'available' && updates.notes}<p class="mt-2 text-sm leading-relaxed whitespace-pre-line text-muted">{updates.notes}</p>{/if}
          {#if updates.error && (updates.status === 'error' || updates.status === 'restart')}<p class="mt-1.5 text-sm break-words text-muted">{updates.error}</p>{/if}
        </div>
      </section>
      {/if}
    </div>
  </div>
</div>
