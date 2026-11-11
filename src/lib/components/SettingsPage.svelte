<script lang="ts">
  import { ArrowLeft, Bot, KeyRound, Eye, EyeOff, Lock, Gauge, ShieldCheck, Palette, Sun, Moon, Monitor, Check, LoaderCircle, X, CircleArrowUp, Image, AppWindow, Square, SlidersHorizontal } from '@lucide/svelte';
  import { ui, ai, db, providers, engines, envTag, canWrite, aiSettings, saveAiSoon, saveAiKeySoon, flushAi, removeAiKey, type Provider, type Conn } from '../state.svelte';
  import { look, canSeeThrough, backdropOn, lookChanged, setTheme, setBackgroundImage, removeBackgroundImage, resetBackdrop, type Backdrop } from '../appearance.svelte';
  import { api, ApiError } from '../api';
  import { appVersion, updates, checkForUpdates, installUpdate, restartApp, updatesEnabled as canUpdate } from '../updates.svelte';
  import ModelPicker from './ModelPicker.svelte';

  let { onEnableWrites }: { onEnableWrites: (c: Conn) => void } = $props();

  let showKey = $state(false);
  let test = $state<'idle' | 'testing' | 'ok' | 'fail'>('idle');
  let tested = $state<{ models: number; ms: number } | null>(null);
  let testError = $state<ApiError | null>(null);
  // A test that a later edit made out of date must not show its result.
  let testSeq = 0;

  const keyNote = { tauri: 'Stored encrypted on this computer.', bridge: 'Kept in memory while the dev bridge runs.', mock: 'Demo build: no AI backend.' }[api.kind];

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
      const r = await api.testAi(aiSettings(), ai.apiKey || null);
      if (seq !== testSeq) return;
      tested = { models: r.modelCount, ms: r.latencyMs };
      test = 'ok';
    } catch (e) {
      if (seq !== testSeq) return;
      testError = ApiError.from(e);
      test = 'fail';
    }
  }

  function toggleWrites(c: Conn) {
    if (c.aiWrite) c.aiWrite = false;
    else if (canWrite(c)) onEnableWrites(c);
  }

  const sections = [
    { id: 'ai', l: 'AI', i: Bot },
    { id: 'safeguards', l: 'Safeguards', i: ShieldCheck },
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
    <span class="label mb-1 flex items-center">{label}<span class="ml-auto font-mono text-[11.5px] text-faint tabular-nums">{look[key]}{unit}</span></span>
    <input class="h-5 w-full cursor-pointer accent-accent" type="range" {min} {max} bind:value={look[key]} oninput={lookChanged} />
    <span class="mt-0.5 block text-[12px] text-faint">{hint}</span>
  </label>
{/snippet}

<div class="flex h-full flex-col">
  <header class="flex h-12 shrink-0 items-center gap-2.5 px-4">
    <button class="btn btn-ghost -ml-1.5 size-7 p-0" onclick={() => (ui.page = 'query')} aria-label="Back to query"><ArrowLeft size={15} /></button>
    <h1 class="text-[14px] font-semibold">Settings</h1>
    <span class="truncate text-[12.5px] text-faint">AI provider, privacy, safeguards and appearance</span>
  </header>

  <nav class="flex shrink-0 items-end gap-1 overflow-x-auto border-b border-line px-4" aria-label="Settings sections">
    {#each sections as s (s.id)}
      <button
        class="-mb-px flex h-9 shrink-0 items-center gap-1.5 border-b-2 px-2 text-[12.5px] font-medium transition-colors {ui.settingsSection === s.id ? 'border-accent text-ink' : 'border-transparent text-muted hover:text-ink'}"
        aria-current={ui.settingsSection === s.id ? 'page' : undefined}
        onclick={() => (ui.settingsSection = s.id)}><s.i size={13} /> {s.l}</button>
    {/each}
  </nav>

  <div class="flex-1 overflow-auto">
    <div class="mx-auto max-w-[44rem] divide-y divide-line px-6 py-2">
      {#if ui.settingsSection === 'ai'}
      <!-- AI provider -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-[13px] font-semibold"><Bot size={14} class="text-faint" /> AI provider</h2>
          <p class="mt-1 text-[12.5px] leading-relaxed text-muted">Any OpenAI-compatible or Anthropic-compatible endpoint. Your key stays on this computer.</p>
        </div>
        <div class="min-w-0">
          <div class="seg flex-wrap">
            {#each Object.entries(providers) as [id, v] (id)}
              <button class="seg-item {ai.provider === id ? 'seg-on' : ''}" onclick={() => pick(id as Provider)}>{v.label}</button>
            {/each}
          </div>

          <div class="mt-3 grid gap-3 sm:grid-cols-2">
            <label class="block sm:col-span-2">
              <span class="label mb-1.5 block">Base URL</span>
              <input class="input font-mono text-[12.5px]" bind:value={ai.baseUrl} oninput={changed} placeholder={providers[ai.provider].url} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
            </label>
            <label class="block">
              <span class="label mb-1.5 flex min-w-0 items-center gap-1 whitespace-nowrap">API key <span class="min-w-0 truncate font-normal text-faint" title="Optional for local servers">(optional for local servers)</span>
                {#if ai.hasKey}<button class="ml-auto shrink-0 rounded px-1 font-medium text-muted hover:bg-hover hover:text-red-600 dark:hover:text-red-400" onclick={removeKey} title="Delete the saved key">Remove</button>{/if}
              </span>
              <div class="relative">
                <input class="input pr-9 font-mono text-[12.5px]" type={showKey ? 'text' : 'password'} bind:value={ai.apiKey} oninput={keyChanged} placeholder={ai.hasKey ? '•••••••• (saved)' : ai.provider === 'anthropic' ? 'sk-ant-…' : 'sk-…'} title={ai.hasKey ? 'Saved key is hidden. Type to replace it.' : undefined} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
                <button class="absolute inset-y-0 right-1 grid w-7 place-items-center text-faint hover:text-ink" onclick={() => (showKey = !showKey)} aria-label={showKey ? 'Hide key' : 'Show key'}>
                  {#if showKey}<EyeOff size={14} />{:else}<Eye size={14} />{/if}
                </button>
              </div>
            </label>
            <div class="block">
              <span class="label mb-1.5 block">Model</span>
              <ModelPicker variant="field" placeholder={providers[ai.provider].model} onpick={changed} />
            </div>
          </div>

          <div class="mt-3 flex items-center gap-3">
            <span class="flex items-center gap-1.5 text-[12px] text-faint"><KeyRound size={12} /> {keyNote}</span>
            <div class="flex-1"></div>
            {#if test === 'ok' && tested}<span class="flex items-center gap-1 text-[12px] font-medium text-emerald-700 dark:text-emerald-400"><Check size={13} /> Connected · {tested.models.toLocaleString()} {tested.models === 1 ? 'model' : 'models'} · {tested.ms} ms</span>
            {:else if test === 'fail'}<span class="flex items-center gap-1 text-[12px] font-medium text-red-700 dark:text-red-400"><X size={13} /> Failed</span>
            {:else if test === 'testing'}<span class="text-[12px] text-muted">testing…</span>{/if}
            <button class="btn btn-secondary" onclick={runTest} disabled={test === 'testing'}>
              {#if test === 'testing'}<LoaderCircle size={13} class="animate-spin" />{/if} Test
            </button>
          </div>
          {#if test === 'fail' && testError}
            <p class="mt-1.5 text-right text-[12px] break-words text-muted">{testError.message}{#if testError.code === 'model_not_found' && testError.detail}{' '}<span class="text-faint">Models: {testError.detail}</span>{/if}</p>
          {/if}
        </div>
      </section>

      <!-- Privacy -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-[13px] font-semibold"><Lock size={14} class="text-faint" /> What the AI can see</h2>
          <p class="mt-1 text-[12.5px] leading-relaxed text-muted">Choose what leaves this machine when you ask the assistant.</p>
        </div>
        <div class="grid min-w-0 gap-2 sm:grid-cols-2">
          {#each [{ id: 'results', t: 'Schema and query results', d: 'Best answers. Result rows go to the model.' }, { id: 'schema', t: 'Schema only', d: 'Rows never leave this computer. The AI only writes SQL.' }] as o (o.id)}
            <button class="rounded-lg border px-3.5 py-3 text-left transition {ai.privacy === o.id ? 'border-accent/70 bg-accent/6 ring-3 ring-accent/12' : 'border-line hover:bg-hover'}" onclick={() => { ai.privacy = o.id as 'results' | 'schema'; changed(); }}>
              <div class="font-medium">{o.t}</div>
              <div class="mt-0.5 text-[12.5px] leading-snug text-muted">{o.d}</div>
            </button>
          {/each}
        </div>
      </section>

      <!-- Limits -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-[13px] font-semibold"><Gauge size={14} class="text-faint" /> Limits for AI queries</h2>
          <p class="mt-1 text-[12.5px] leading-relaxed text-muted">Caps applied to every query the assistant runs.</p>
        </div>
        <div class="grid min-w-0 gap-3 sm:grid-cols-2">
          <label class="block">
            <span class="label mb-1.5 block">Max rows</span>
            <input class="input font-mono tabular-nums" type="number" min="1" max="10000" bind:value={ai.maxRows} oninput={changed} />
          </label>
          <label class="block">
            <span class="label mb-1.5 block">Timeout (seconds)</span>
            <input class="input font-mono tabular-nums" type="number" min="1" max="300" bind:value={ai.timeoutSec} oninput={changed} />
          </label>
        </div>
      </section>

      {:else if ui.settingsSection === 'safeguards'}
      <!-- Safeguards -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-[13px] font-semibold"><ShieldCheck size={14} class="text-faint" /> Safeguards</h2>
          <p class="mt-1 text-[12.5px] leading-relaxed text-muted">Read-only is on for every connection by default. With write mode on, each write still needs your approval.</p>
        </div>
        <div class="min-w-0 divide-y divide-line rounded-lg border border-line">
          <!-- SQL and MongoDB connections: the chat can propose writes on both, each still needs approval. -->
          {#each db.connections as c (c.id)}
            {@const on = c.aiWrite && canWrite(c)}
            <div class="flex items-center gap-2 px-3 py-2.5">
              <span class="w-5 shrink-0 font-mono text-[10.5px] font-semibold {engines[c.engine].badge}">{engines[c.engine].short}</span>
              <span class="truncate font-medium">{c.name}</span>
              <span class="tag {envTag[c.env]}">{c.env}</span>
              <div class="flex-1"></div>
              {#if canWrite(c)}
                <span class="text-[12px] {on ? 'text-amber-600 dark:text-amber-400' : 'text-muted'}">{on ? 'Writes need approval' : 'Read-only'}</span>
              {:else}
                <span class="text-[12px] text-muted">Connection is read-only</span>
              {/if}
              <button
                role="switch"
                aria-checked={on}
                aria-label="AI write mode for {c.name}"
                class="relative h-5 w-9 shrink-0 rounded-full transition disabled:cursor-not-allowed disabled:opacity-50 {on ? 'bg-amber-500' : 'bg-stone-500'}"
                disabled={!canWrite(c)}
                title={canWrite(c) ? undefined : 'Turn off read-only in the connection settings to allow AI writes'}
                onclick={() => toggleWrites(c)}>
                <span class="absolute top-0.5 size-4 rounded-full bg-white shadow transition-all {on ? 'left-[18px]' : 'left-0.5'}"></span>
              </button>
            </div>
          {/each}
        </div>
      </section>

      {:else if ui.settingsSection === 'appearance'}
      <!-- Theme -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-[13px] font-semibold"><Palette size={14} class="text-faint" /> Theme</h2>
          <p class="mt-1 text-[12.5px] leading-relaxed text-muted">Colors for the whole app.</p>
        </div>
        <div class="min-w-0">
          <div class="seg">
            {#each [{ id: 'light', l: 'Light', i: Sun }, { id: 'dark', l: 'Dark', i: Moon }, { id: 'system', l: 'System', i: Monitor }] as t (t.id)}
              <button
                class="seg-item flex items-center gap-1.5 {ui.theme === t.id ? 'seg-on' : ''}"
                onclick={() => setTheme(t.id as 'light' | 'dark' | 'system')}><t.i size={13} /> {t.l}</button>
            {/each}
          </div>
        </div>
      </section>

      <!-- Background -->
      <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
        <div>
          <h2 class="flex items-center gap-2 text-[13px] font-semibold"><Image size={14} class="text-faint" /> Background</h2>
          <p class="mt-1 text-[12.5px] leading-relaxed text-muted">
            Show an image behind the app{#if canSeeThrough}, or make the window see-through{/if}. The image stays on this computer.
          </p>
        </div>
        <div class="min-w-0">
          <div class="seg">
            {#each backdrops as b (b.id)}
              <button class="seg-item flex items-center gap-1.5 {look.backdrop === b.id ? 'seg-on' : ''}" onclick={() => pickBackdrop(b.id)}><b.i size={13} /> {b.l}</button>
            {/each}
          </div>
          <input bind:this={imageInput} class="hidden" type="file" accept="image/*" onchange={imagePicked} />

          {#if look.backdrop === 'image'}
            <div class="mt-3 flex items-center gap-3">
              {#if look.image}
                <div class="h-14 w-24 shrink-0 rounded-md border border-line bg-cover bg-center" style:background-image="url({JSON.stringify(look.image)})" role="img" aria-label="Current background image"></div>
              {:else}
                <div class="grid h-14 w-24 shrink-0 place-items-center rounded-md border border-dashed border-line text-faint"><Image size={16} /></div>
              {/if}
              <button class="btn btn-secondary" onclick={() => imageInput?.click()} disabled={imageBusy}>
                {#if imageBusy}<LoaderCircle size={13} class="animate-spin" />{/if} {look.image ? 'Change image…' : 'Choose image…'}
              </button>
              {#if look.image}<button class="btn btn-ghost hover:text-red-600 dark:hover:text-red-400" onclick={removeBackgroundImage}>Remove</button>{/if}
            </div>
            {#if imageError}<p class="mt-1.5 text-[12px] text-red-700 dark:text-red-400">{imageError}</p>{/if}
          {:else if look.backdrop === 'desktop'}
            <div class="mt-3 flex items-center gap-2.5">
              <button
                role="switch"
                aria-checked={look.vibrancy}
                aria-label="Blur the desktop behind the window"
                class="relative h-5 w-9 shrink-0 rounded-full transition {look.vibrancy ? 'bg-accent' : 'bg-stone-500'}"
                onclick={() => { look.vibrancy = !look.vibrancy; lookChanged(); }}>
                <span class="absolute top-0.5 size-4 rounded-full bg-white shadow transition-all {look.vibrancy ? 'left-[18px]' : 'left-0.5'}"></span>
              </button>
              <span class="text-[12.5px]">Blur the desktop behind the window</span>
            </div>
          {/if}
        </div>
      </section>

      {#if backdropOn()}
        <!-- Transparency -->
        <section class="grid gap-x-10 gap-y-3 py-6 sm:grid-cols-[180px_1fr]">
          <div>
            <h2 class="flex items-center gap-2 text-[13px] font-semibold"><SlidersHorizontal size={14} class="text-faint" /> Transparency</h2>
            <p class="mt-1 text-[12.5px] leading-relaxed text-muted">How much of the background shows through. Lower values are harder to read.</p>
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
          <h2 class="flex items-center gap-2 text-[13px] font-semibold"><CircleArrowUp size={14} class="text-faint" /> Updates</h2>
          <p class="mt-1 text-[12.5px] leading-relaxed text-muted">sandb asks before it installs anything.</p>
        </div>
        <div class="min-w-0">
          <div class="flex items-center gap-3">
            <span class="font-mono text-[12.5px]">sandb v{appVersion}</span>
            <div class="flex-1"></div>
            {#if updates.status === 'latest'}<span class="flex items-center gap-1 text-[12px] font-medium text-emerald-700 dark:text-emerald-400"><Check size={13} /> Up to date</span>
            {:else if updates.status === 'available'}<span class="text-[12px] font-medium text-amber-600 dark:text-amber-400">v{updates.next} is available</span>
            {:else if updates.status === 'error'}<span class="flex items-center gap-1 text-[12px] font-medium text-red-700 dark:text-red-400"><X size={13} /> Check failed</span>{/if}
            {#if updates.status === 'restart'}
              <span class="text-[12px] font-medium text-amber-600 dark:text-amber-400">v{updates.next} is installed</span>
              <button class="btn btn-primary" onclick={restartApp}>Restart to finish the update</button>
            {:else if updates.status === 'available' || updates.status === 'installing'}
              <button class="btn btn-primary" onclick={installUpdate} disabled={updates.status === 'installing'}>
                {#if updates.status === 'installing'}<LoaderCircle size={13} class="animate-spin" /> Installing…{:else}Install and restart{/if}
              </button>
            {:else}
              <button class="btn btn-secondary" onclick={() => checkForUpdates()} disabled={!canUpdate || updates.status === 'checking'} title={canUpdate ? undefined : 'Updates work in the installed desktop app only'}>
                {#if updates.status === 'checking'}<LoaderCircle size={13} class="animate-spin" />{/if} Check for updates
              </button>
            {/if}
          </div>
          {#if updates.status === 'available' && updates.notes}<p class="mt-2 text-[12.5px] leading-relaxed whitespace-pre-line text-muted">{updates.notes}</p>{/if}
          {#if updates.error && (updates.status === 'error' || updates.status === 'restart')}<p class="mt-1.5 text-[12px] break-words text-muted">{updates.error}</p>{/if}
        </div>
      </section>
      {/if}
    </div>
  </div>
</div>
