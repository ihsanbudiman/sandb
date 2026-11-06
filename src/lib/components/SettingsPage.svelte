<script lang="ts">
  import { ArrowLeft, ArrowLeftRight, ExternalLink, LogOut, Bot, KeyRound, Eye, EyeOff, Lock, Gauge, ShieldCheck, Palette, Sun, Moon, Monitor, Check, LoaderCircle, X, CircleArrowUp, Image, AppWindow, Square, SlidersHorizontal } from '@lucide/svelte';
  import { ui, ai, db, providers, engines, envTag, canWrite, aiSettings, saveAiSoon, saveAiKeySoon, flushAi, removeAiKey, chatgpt, pickProvider, signInChatGpt, cancelChatGptSignIn, logoutChatGpt, manageChatGptUsage, dismissChatGptWelcome, chatGptIssue, type Provider, type Conn } from '../state.svelte';
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

  function pick(id: Provider) {
    testSeq++;
    test = 'idle';
    pickProvider(id);
  }

  let welcomeDialog = $state<HTMLDialogElement>();
  $effect(() => {
    if (chatgpt.welcome && welcomeDialog && !welcomeDialog.open) welcomeDialog.showModal();
    else if (!chatgpt.welcome && welcomeDialog?.open) welcomeDialog.close();
  });

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
      const r = await api.testAi(aiSettings(), ai.provider === 'chatgpt' ? null : ai.apiKey || null);
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
          <p class="mt-1 text-[12.5px] leading-relaxed text-muted">Use your ChatGPT plan, or your own OpenAI-compatible or Anthropic-compatible endpoint. Credentials stay on this computer.</p>
        </div>
        <div class="min-w-0">
          <div class="seg flex-wrap">
            {#each Object.entries(providers) as [id, v] (id)}
              <button class="seg-item {ai.provider === id ? 'seg-on' : ''}" disabled={chatgpt.pending || chatgpt.signingOut} onclick={() => pick(id as Provider)}>{v.label}</button>
            {/each}
          </div>

          {#if ai.provider === 'chatgpt'}
            <div class="mt-3 space-y-3" aria-busy={chatgpt.pending || chatgpt.signingOut}>
              {#if !chatgpt.supported}
                <p class="text-[12.5px] text-muted" role="status">ChatGPT sign-in and plan usage require the native desktop app. The browser demo and dev bridge do not support OAuth.</p>
              {/if}
              <div class={chatgpt.signedIn ? 'rounded-xl border border-line' : 'space-y-3'}>
                {#if chatgpt.signedIn}
                  <div class="flex flex-wrap items-center gap-3 p-4">
                    <span class="grid size-10 shrink-0 place-items-center rounded-xl border border-line bg-sunken" aria-hidden="true"><Bot size={20} strokeWidth={1.5} /></span>
                    <div class="min-w-0 flex-1">
                      <h3 class="font-semibold">ChatGPT plan</h3>
                      <p class="mt-0.5 text-[12px] break-all text-muted">{chatgpt.accountLabel || 'ChatGPT account'}</p>
                    </div>
                    <span class="flex items-center gap-1.5 text-[11.5px] {chatgpt.usageEnabled && !chatgpt.loading && !chatgpt.pending && !chatgpt.signingOut ? 'text-emerald-700 dark:text-emerald-400' : 'text-muted'}" role="status" aria-live="polite">
                      {#if chatgpt.loading}Checking ChatGPT sign-in…
                      {:else if chatgpt.pending}Complete sign-in in your system browser…
                      {:else if chatgpt.signingOut}Signing out…
                      {:else if chatgpt.usageEnabled}<span class="size-1.5 shrink-0 rounded-full bg-current" aria-hidden="true"></span>Connected
                      {:else}Plan usage is not enabled{/if}
                    </span>
                  </div>
                  <div class="space-y-2 border-t border-line p-4">
                    <div><span class="label mb-1.5 block">Account model</span><ModelPicker variant="field" placeholder="Choose an account model" onpick={changed} /></div>
                    {#if chatgpt.usageEnabled}
                      <p class="text-[11.5px] text-muted">Requests use this account’s plan or credits.</p>
                    {:else if !chatgpt.pending}
                      <p class="text-[12.5px] text-muted">Sign-in alone does not permit inference. Enable ChatGPT plan usage, or explicitly choose an API-key provider.</p>
                      <button class="btn btn-primary" onclick={() => signInChatGpt(true)} disabled={!chatgpt.supported || chatgpt.loading || chatgpt.signingOut}>Enable ChatGPT plan usage</button>
                    {/if}
                  </div>
                {:else if chatgpt.supported}
                  <p class="text-[12.5px] text-muted" role="status" aria-live="polite">
                    {#if chatgpt.loading}Checking ChatGPT sign-in…
                    {:else if chatgpt.pending}Complete sign-in in your system browser…
                    {:else if chatgpt.signingOut}Signing out…
                    {:else}Not signed in to ChatGPT.{/if}
                  </p>
                {/if}
                <div class="flex flex-wrap items-center gap-2 {chatgpt.signedIn ? 'rounded-b-xl border-t border-line bg-sunken px-3 py-2.5' : ''}">
                {#if chatgpt.pending}
                  <LoaderCircle size={14} class="animate-spin text-muted" />
                  <button class="btn btn-secondary" onclick={cancelChatGptSignIn} disabled={chatgpt.cancelling}>Cancel sign-in</button>
                {:else if chatgpt.signedIn}
                  <button class="btn btn-secondary" onclick={() => signInChatGpt()} disabled={!chatgpt.supported || chatgpt.loading || chatgpt.signingOut} title="Sign in with another ChatGPT account in your browser"><ArrowLeftRight size={13} /> Switch account</button>
                {:else}
                  <button type="button" data-oai-siwc-button class="flex h-[45px] w-[242px] max-w-full items-center justify-center gap-3 rounded-xl bg-black px-5 text-[15px] font-medium text-white disabled:opacity-50" onclick={() => signInChatGpt()} disabled={!chatgpt.supported || chatgpt.loading || chatgpt.signingOut}>
                    <!-- Official SIWC vector is inline to keep sign-in independent of remote image loading. -->
                    <svg aria-hidden="true" class="shrink-0" width="21" height="21" viewBox="0 0 21 21" fill="none"><path d="M11.8102 19.1625C11.2327 19.1625 10.6841 19.0528 10.1643 18.8334C9.64459 18.6139 9.18259 18.3078 8.77834 17.9151C8.33944 18.0653 7.88322 18.1404 7.40967 18.1404C6.63582 18.1404 5.91972 17.9498 5.26137 17.5686C4.60302 17.1875 4.07172 16.6677 3.66747 16.0094C3.27477 15.351 3.07842 14.6176 3.07842 13.8091C3.07842 13.4742 3.12462 13.1103 3.21702 12.7176C2.75502 12.2903 2.39697 11.7994 2.14287 11.245C1.88877 10.679 1.76172 10.09 1.76172 9.47785C1.76172 8.85415 1.89454 8.25355 2.16019 7.67605C2.42584 7.09855 2.79544 6.6019 3.26899 6.1861C3.75409 5.75875 4.31427 5.46422 4.94952 5.30252C5.07657 4.64417 5.34222 4.05512 5.74647 3.53537C6.16227 3.00407 6.67047 2.58827 7.27107 2.28797C7.87167 1.98767 8.51269 1.83752 9.19414 1.83752C9.77164 1.83752 10.3203 1.94725 10.84 2.1667C11.3598 2.38615 11.8218 2.69222 12.226 3.08492C12.6649 2.93477 13.1211 2.8597 13.5947 2.8597C14.3685 2.8597 15.0846 3.05027 15.743 3.43142C16.4013 3.81257 16.9269 4.33232 17.3196 4.99067C17.7238 5.64902 17.9259 6.38245 17.9259 7.19095C17.9259 7.5259 17.8797 7.88973 17.7873 8.28242C18.2493 8.70977 18.6074 9.20643 18.8615 9.77238C19.1156 10.3268 19.2426 10.91 19.2426 11.5222C19.2426 12.1459 19.1098 12.7465 18.8442 13.324C18.5785 13.9015 18.2031 14.4039 17.718 14.8313C17.2445 15.2471 16.6901 15.5358 16.0548 15.6975C15.9278 16.3559 15.6564 16.9449 15.2406 17.4647C14.8363 17.996 14.3339 18.4118 13.7333 18.7121C13.1327 19.0124 12.4917 19.1625 11.8102 19.1625ZM7.53094 16.9969C8.10844 16.9969 8.61087 16.8756 9.03822 16.6331L12.2953 14.762C12.4108 14.6811 12.4686 14.5714 12.4686 14.4328V12.9429L8.27592 15.351C8.02182 15.5012 7.76772 15.5012 7.51362 15.351L4.23919 13.4626C4.23919 13.4973 4.23342 13.5377 4.22187 13.5839C4.22187 13.6301 4.22187 13.6994 4.22187 13.7918C4.22187 14.3808 4.36047 14.9237 4.63767 15.4203C4.92642 15.9054 5.32489 16.2866 5.83309 16.5638C6.34129 16.8525 6.90724 16.9969 7.53094 16.9969ZM7.70419 14.1729C7.77349 14.2076 7.83702 14.2249 7.89477 14.2249C7.95252 14.2249 8.01027 14.2076 8.06802 14.1729L9.36739 13.4279L5.19207 11.0024C4.93797 10.8523 4.81092 10.6271 4.81092 10.3268V6.56725C4.23342 6.82135 3.77142 7.21405 3.42492 7.74535C3.07842 8.2651 2.90517 8.8426 2.90517 9.47785C2.90517 10.0438 3.04954 10.5866 3.33829 11.1064C3.62704 11.6261 4.00242 12.0189 4.46442 12.2845L7.70419 14.1729ZM11.8102 18.0191C12.4224 18.0191 12.9768 17.8805 13.4734 17.6033C13.9701 17.3261 14.3628 16.9449 14.6515 16.4598C14.9403 15.9747 15.0846 15.4319 15.0846 14.8313V11.0891C15.0846 10.9505 15.0269 10.8465 14.9114 10.7772L13.5947 10.0149V14.8486C13.5947 15.1489 13.4676 15.3741 13.2135 15.5243L9.93912 17.4127C10.5051 17.817 11.1288 18.0191 11.8102 18.0191ZM12.4686 11.6781V9.32192L10.5108 8.21312L8.53579 9.32192V11.6781L10.5108 12.7869L12.4686 11.6781ZM7.40967 6.15145C7.40967 5.85115 7.53672 5.62592 7.79082 5.47577L11.0652 3.58735C10.4993 3.1831 9.87559 2.98097 9.19414 2.98097C8.58199 2.98097 8.02759 3.11957 7.53094 3.39677C7.03429 3.67397 6.64159 4.05512 6.35284 4.54022C6.07564 5.02532 5.93704 5.56817 5.93704 6.16878V9.89365C5.93704 10.0323 5.99479 10.142 6.11029 10.2228L7.40967 10.9851V6.15145ZM16.2108 14.4328C16.7883 14.1787 17.2445 13.786 17.5794 13.2547C17.9259 12.7234 18.0992 12.1459 18.0992 11.5222C18.0992 10.9562 17.9548 10.4134 17.6661 9.89365C17.3773 9.3739 17.0019 8.9812 16.5399 8.71555L13.3002 6.84445C13.2309 6.79825 13.1673 6.78092 13.1096 6.79248C13.0518 6.79248 12.9941 6.8098 12.9363 6.84445L11.637 7.5721L15.8296 10.0149C15.9567 10.0842 16.0491 10.1766 16.1068 10.2921C16.1761 10.3961 16.2108 10.5231 16.2108 10.6733V14.4328ZM12.7284 5.6317C12.9825 5.47 13.2366 5.47 13.4907 5.6317L16.7825 7.55477C16.7825 7.47393 16.7825 7.36997 16.7825 7.24292C16.7825 6.68852 16.6439 6.163 16.3667 5.66635C16.101 5.15815 15.7141 4.7539 15.2059 4.4536C14.7093 4.1533 14.1318 4.00315 13.4734 4.00315C12.8959 4.00315 12.3935 4.12442 11.9661 4.36697L8.70904 6.23808C8.59354 6.31892 8.53579 6.42865 8.53579 6.56725V8.0572L12.7284 5.6317Z" fill="white" /></svg>
                    Continue with ChatGPT
                  </button>
                {/if}
                  <div class="ml-auto flex flex-wrap items-center gap-1">
                    <button class="btn btn-ghost" onclick={manageChatGptUsage} disabled={!chatgpt.supported}>Manage usage <ExternalLink size={12} /></button>
                    {#if chatgpt.signedIn && !chatgpt.pending}<button class="btn btn-ghost" onclick={logoutChatGpt} disabled={chatgpt.loading || chatgpt.signingOut}><LogOut size={12} /> Sign out</button>{/if}
                  </div>
                </div>
              </div>
              <details class="text-[12px] text-muted" open={!chatgpt.signedIn}>
                <summary class="w-fit cursor-pointer rounded py-1 hover:text-ink">About ChatGPT plan</summary>
                <p class="mt-2 leading-relaxed">Eligible requests use your ChatGPT plan or credits balance. Preview access requires an eligible Plus or Pro account. sandb never silently switches to paid API credentials.</p>
              </details>
              {#if chatgpt.warning}<p class="text-[12.5px] text-amber-700 dark:text-amber-400" role="status">{chatgpt.warning}</p>{/if}
              {#if chatgpt.error}
                {@const issue = chatGptIssue(chatgpt.error)}
                <p class="text-[12.5px] text-red-700 dark:text-red-400" role="alert">{issue.text}</p>
                {#if issue.action === 'usage'}<button class="btn btn-primary" onclick={manageChatGptUsage}>Manage usage</button>{/if}
                {#if issue.diagnostic || issue.requestId}<p class="text-[11.5px] break-all text-muted">{issue.diagnostic}{#if issue.requestId} · Request ID: {issue.requestId}{/if}</p>{/if}
              {/if}
            </div>
          {:else}
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

<dialog bind:this={welcomeDialog} class="dialog m-auto max-w-md backdrop:bg-black/40" style="position: fixed; inset: 0;" aria-labelledby="chatgpt-welcome-title" aria-describedby="chatgpt-welcome-description" oncancel={dismissChatGptWelcome} onclose={dismissChatGptWelcome}>
  <header class="dialog-head"><h2 id="chatgpt-welcome-title">You're using your ChatGPT plan</h2></header>
  <div class="dialog-body">
    <p id="chatgpt-welcome-description" class="leading-relaxed text-muted">Eligible AI requests in sandb use your ChatGPT plan or credits balance. Your existing database tools, privacy controls, and write safeguards still apply. Manage usage in ChatGPT settings.</p>
  </div>
  <footer class="dialog-foot">
    <button class="btn btn-secondary" onclick={manageChatGptUsage}>Manage usage</button>
    <!-- svelte-ignore a11y_autofocus -->
    <button class="btn btn-primary" autofocus onclick={dismissChatGptWelcome}>Got it</button>
  </footer>
</dialog>
