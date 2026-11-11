<script lang="ts">
  import { X, LoaderCircle, KeyRound } from '@lucide/svelte';
  import { untrack } from 'svelte';
  import { db, active, engines, envDot, toConn, type Conn, type Engine, type Env } from '../state.svelte';
  import { activeTab, setTabConn } from '../tabs.svelte';
  import { api, ApiError, type Connection, type ConnectionInput, type SslMode, type Topology } from '../api';
  import { isMongoUri, parseMongoUri } from '../mongoUri';
  import SafeguardDialog from './SafeguardDialog.svelte';

  // `conn` set: edit it, or, if its id is not in db.connections yet, save it as a new copy (duplicate).
  let {
    open = $bindable(false),
    conn = null,
    onsaved,
    onedited,
    canswitch = () => true,
    canupdate = () => true,
  }: {
    open?: boolean;
    conn?: Conn | null;
    onsaved: () => void;
    onedited?: (c: Conn, dbChanged: boolean) => void;
    canswitch?: () => boolean | Promise<boolean>;
    canupdate?: (c: Conn, dbChanged: boolean) => boolean | Promise<boolean>;
  } = $props();

  let engine = $state<Engine>('pg');
  let name = $state('');
  let host = $state('localhost');
  let port = $state(5432);
  let database = $state('');
  let user = $state('postgres');
  let password = $state('');
  let ssl = $state<SslMode>('prefer');
  let file = $state('~/data/app.db');
  let env = $state<Env>('local');
  let aiWrite = $state(false);
  let readOnly = $state(false);
  // MongoDB only: connect with the fields or with a connection string (`uri`, sent as typed, password included).
  let mode = $state<'fields' | 'uri'>('fields');
  let uri = $state('');
  let authSource = $state('');
  let test = $state<'idle' | 'testing' | 'ok' | 'error'>('idle');
  let testMsg = $state('');
  // Version and topology from the last successful test, valid only while the connection fields match `testedSig`.
  let tested: { sig: string; version: string; topology: Topology | null } | null = null;
  // Saved connections keep their stored password unless the user types a new one.
  let passwordDirty = $state(false);
  let saving = $state(false);
  let saveError = $state('');
  // Bumped on every reset, so a test that finishes after close/reopen is ignored.
  let run = 0;
  let guard = $state<Conn | null>(null);
  let guardCopy: Conn | null = null;

  const existing = $derived(conn ? db.connections.find((x) => x.id === conn.id) : undefined);
  const mongo = $derived(engine === 'mongo');
  const useUri = $derived(mongo && mode === 'uri');

  $effect(() => {
    // Reading `conn` re-fills when ⌘K "New connection" replaces an open edit.
    if (open) {
      conn;
      untrack(() => fill(conn));
    }
  });

  // SafeguardDialog sets aiWrite on the copy when the user confirms; the real connection changes only on Save.
  $effect(() => {
    if (!guard && guardCopy) {
      if (guardCopy.aiWrite) aiWrite = true;
      guardCopy = null;
    }
  });

  function fill(c: Conn | null) {
    if (!c) {
      // A new connection always starts from a clean draft, never from the last edited or saved one.
      setEngine('pg');
      name = database = password = '';
      host = 'localhost';
      ssl = 'prefer';
      file = '~/data/app.db';
      env = 'local';
      aiWrite = false;
      readOnly = false;
      mode = 'fields';
      uri = authSource = '';
      resetStatus();
      return;
    }
    engine = c.engine;
    name = c.name;
    database = c.database;
    host = c.host || 'localhost';
    port = c.port || engines[c.engine].port;
    user = c.user;
    // The stored password is never sent to the UI. The mock shows fake dots like the prototype did.
    password = api.kind === 'mock' && c.engine !== 'sqlite' ? 'saved-secret' : '';
    ssl = c.ssl;
    file = c.file || '~/data/app.db';
    env = c.env;
    aiWrite = c.aiWrite;
    readOnly = c.readOnly;
    // The saved string has no password; the stored one is kept unless the user types one into it.
    uri = c.uri;
    mode = c.uri ? 'uri' : 'fields';
    authSource = c.authSource;
    resetStatus();
  }

  function resetStatus() {
    run++;
    test = 'idle';
    testMsg = '';
    tested = null;
    passwordDirty = false;
    saving = false;
    saveError = '';
  }

  function toggleAi() {
    if (aiWrite) return (aiWrite = false);
    if (readOnly) return;
    // A new connection has no Conn yet; the stub only feeds SafeguardDialog.
    const base: Conn = conn ?? toConn({ ...input(), id: '', name: 'new connection', version: '', aiWrite: false, topology: null });
    guard = { ...base, name: name.trim() || base.name, env, readOnly, aiWrite: false };
    guardCopy = guard;
  }

  // A read-only connection refuses AI writes too, so turning it on also turns AI write access off.
  function toggleReadOnly() {
    readOnly = !readOnly;
    if (readOnly) aiWrite = false;
  }

  const engineList: Engine[] = ['pg', 'mysql', 'sqlite', 'mongo'];
  const envs: Env[] = ['local', 'staging', 'production'];
  // MongoDB needs no database: a server lists its own.
  const canSave = $derived(name.trim().length > 0 && (engine === 'sqlite' ? file.trim() : useUri ? isMongoUri(uri) : host.trim() && (mongo || database.trim())));

  function setEngine(e: Engine) {
    // MongoDB has no Prefer TLS mode; it starts with Disable.
    if (e === 'mongo') ssl = 'disable';
    else if (engine === 'mongo') ssl = 'prefer';
    engine = e;
    port = engines[e].port;
    user = e === 'pg' ? 'postgres' : e === 'mysql' ? 'root' : '';
    test = 'idle';
  }

  function input(): ConnectionInput & Pick<Connection, 'uri' | 'authSource'> {
    return { name: name.trim(), engine, env, host: host.trim(), port: Number(port) || 0, database: database.trim(), user: user.trim(), ssl, file: file.trim(), aiWrite, readOnly, uri: useUri ? uri.trim() : '', authSource: mongo && !useUri ? authSource.trim() : '' };
  }

  // Fields that decide where the connection goes; a test result is stale once any of them changes.
  const sig = $derived(JSON.stringify([engine, useUri, host, port, database, user, ssl, file, password, uri, authSource]));
  $effect(() => {
    sig;
    untrack(() => {
      if (test === 'ok' || test === 'error') test = 'idle';
    });
  });

  // null = keep the stored secret (edit) or the source's secret (duplicate).
  function passwordArg(): string | null {
    if (engine === 'sqlite') return existing ? null : '';
    // A password typed into the string wins over this argument; without one the stored password is kept.
    if (useUri) return null;
    if (conn && !passwordDirty) return null;
    return password;
  }

  // The id whose stored password a test may fall back to. SQLite has no password to fall back to. A changed
  // connection string tests without it: its password, if the server needs one, goes into the string.
  const secretId = () => (conn && !passwordDirty && engine !== 'sqlite' && !(useUri && targetChanged()) ? (existing ? conn.id : (conn.copyOf ?? null)) : null);

  // The backend refuses to send a stored password to another target; say so before the round trip.
  function targetChanged(): boolean {
    const src = conn && db.connections.find((x) => x.id === (existing ? conn.id : conn.copyOf));
    if (!src) return false;
    if (src.engine !== engine) return true;
    // The saved string has no password, so the typed one is compared without it.
    if (src.uri !== (useUri ? (parseMongoUri(uri)?.redacted ?? uri.trim()) : '')) return true;
    return !useUri && (src.host !== host.trim() || src.port !== (Number(port) || engines[engine].port) || src.user !== user.trim());
  }
  const needsPassword = $derived(api.kind !== 'mock' && engine !== 'sqlite' && !useUri && !!conn && !passwordDirty && targetChanged());

  function errText(e: unknown) {
    const err = ApiError.from(e);
    return err.detail ? `${err.message}: ${err.detail}` : err.message;
  }

  async function runTest() {
    if (needsPassword) {
      testMsg = 'Enter the password to test changed connection settings';
      test = 'error';
      return;
    }
    test = 'testing';
    testMsg = '';
    const at = sig;
    const mine = run;
    try {
      const r = await api.testConnection(input(), passwordArg(), secretId());
      if (mine !== run || at !== sig || test !== 'testing') return;
      tested = { sig: at, version: r.serverVersion, topology: r.topology ?? null };
      testMsg = `Connected · ${r.serverVersion}${r.topology ? ` · ${r.topology.replace('_', ' ')}` : ''} · ${r.latencyMs} ms`;
      test = 'ok';
    } catch (e) {
      if (mine !== run || at !== sig || test !== 'testing') return;
      testMsg = errText(e);
      test = 'error';
    }
  }

  function close() {
    open = false;
    password = '';
    resetStatus();
  }

  function payload(): ConnectionInput {
    const fresh = tested && tested.sig === sig ? tested : null;
    return { ...input(), version: fresh ? fresh.version : null, topology: fresh ? fresh.topology : null };
  }

  async function save() {
    if (saving) return;
    if (conn) return saveConn(conn);
    if (!(await canswitch())) return;
    saving = true;
    saveError = '';
    try {
      const c = toConn(await api.createConnection(payload(), passwordArg()));
      db.connections.push(c);
      const t = activeTab();
      if (t) setTabConn(t, c);
    } catch (e) {
      saveError = errText(e);
      saving = false;
      return;
    }
    name = '';
    database = '';
    aiWrite = false;
    readOnly = false;
    close();
    onsaved();
  }

  async function saveConn(c: Conn) {
    const cur = existing;
    if (!cur) {
      if (!(await canswitch())) return;
      saving = true;
      saveError = '';
      try {
        const src = c.copyOf;
        const saved = toConn(src ? await api.duplicateConnection(src, payload(), passwordArg()) : await api.createConnection(payload(), passwordArg()));
        db.connections.push(saved);
        const t = activeTab();
        if (t) setTabConn(t, saved);
      } catch (e) {
        saveError = errText(e);
        saving = false;
        return;
      }
      close();
      onsaved();
      return;
    }
    const next = payload();
    const nextDb = engine === 'sqlite' ? 'main' : next.database;
    // Only a change of target touches the open result; name, env and AI access do not.
    const dbChanged = cur.database !== nextDb || cur.host !== (engine === 'sqlite' ? '' : next.host) || cur.port !== (engine === 'sqlite' ? 0 : next.port) || cur.file !== (engine === 'sqlite' ? next.file : '');
    if (dbChanged && cur.id === active().id && !(await canswitch())) return;
    if (!(await canupdate(cur, dbChanged))) return;
    saving = true;
    saveError = '';
    try {
      const saved = await api.updateConnection(cur.id, next, passwordArg());
      Object.assign(cur, toConn(saved));
    } catch (e) {
      saveError = errText(e);
      saving = false;
      return;
    }
    close();
    onedited?.(cur, dbChanged);
  }
</script>

<svelte:window onkeydown={(e) => open && !guard && e.key === 'Escape' && close()} />

{#snippet field(text: string)}
  <span class="label mb-1.5 block">{text}</span>
{/snippet}

{#if open}
  <div class="dialog-backdrop">
    <div class="dialog flex max-h-[calc(100dvh-2rem)] w-full max-w-lg flex-col">
      <header class="dialog-head">
        <h2 class="flex-1">{!conn ? 'New connection' : existing ? 'Edit connection' : 'Duplicate connection'}</h2>
        <button class="btn btn-ghost -my-1 -mr-1.5 size-7 p-0" onclick={close} aria-label="Close"><X size={15} /></button>
      </header>

      <div class="dialog-body flex-1 space-y-4 overflow-auto pb-5">
        <p class="-mt-2 text-[12.5px] text-muted">{conn ? `${engines[engine].label} · the engine of a saved connection cannot change.` : 'Connect to a PostgreSQL, MySQL, SQLite or MongoDB database.'}</p>

        <div class="grid grid-cols-4 gap-2">
          {#each engineList as e}
            <button
              class="flex flex-col items-start gap-1 rounded-lg border px-3 py-2.5 text-left transition disabled:cursor-not-allowed {engine === e ? 'border-accent/70 bg-accent/6 ring-3 ring-accent/12' : conn ? 'border-line opacity-40' : 'border-line hover:bg-hover'}"
              disabled={!!conn}
              title={conn && engine !== e ? 'Engine cannot be changed' : undefined}
              onclick={() => setEngine(e)}
            >
              <span class="font-mono text-[10.5px] font-semibold {engines[e].badge}">{engines[e].short}</span>
              <span class="text-[13px] font-medium">{engines[e].label}</span>
            </button>
          {/each}
        </div>

        <label class="block">{@render field('Name')}<input class="input" bind:value={name} placeholder="e.g. billing-prod" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>

        <!-- Above the target fields so Environment and Read-only stay in view on short windows; the body still scrolls. -->
        <div class="space-y-2">
          <div>
            {@render field('Environment')}
            <div class="seg flex">
              {#each envs as e}
                <button class="seg-item flex flex-1 items-center justify-center gap-1.5 capitalize {env === e ? 'seg-on' : ''}" onclick={() => (env = e)}>
                  <span class="size-1.5 rounded-full {envDot[e]}"></span>{e}
                </button>
              {/each}
            </div>
          </div>
          <div class="flex items-center gap-3 pt-1">
            <div class="flex-1">
              <div class="font-medium">Read-only</div>
              <div class="text-[12.5px] text-muted">Blocks all writes, including your own queries</div>
            </div>
            <button
              role="switch"
              aria-checked={readOnly}
              aria-label="Read-only connection"
              class="relative h-5 w-9 shrink-0 rounded-full transition {readOnly ? 'bg-accent' : 'bg-stone-500'}"
              onclick={toggleReadOnly}
            >
              <span class="absolute top-0.5 left-0.5 size-4 rounded-full bg-white shadow transition {readOnly ? 'translate-x-4' : ''}"></span>
            </button>
          </div>
          {#if mongo && readOnly}<p class="text-[12px] text-amber-700 dark:text-amber-400">Use a database user with the read role for full protection.</p>{/if}
        </div>

        {#if engine === 'sqlite'}
          <label class="block">{@render field('Database file')}<input class="input font-mono" bind:value={file} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
        {:else}
          {#if mongo}
            <div>
              {@render field('Connect with')}
              <div class="seg flex">
                <button class="seg-item flex flex-1 items-center justify-center {mode === 'fields' ? 'seg-on' : ''}" aria-pressed={mode === 'fields'} onclick={() => (mode = 'fields')}>Fields</button>
                <button class="seg-item flex flex-1 items-center justify-center {mode === 'uri' ? 'seg-on' : ''}" aria-pressed={mode === 'uri'} onclick={() => (mode = 'uri')}>Connection string</button>
              </div>
            </div>
          {/if}
          {#if useUri}
            <label class="block">{@render field('Connection string')}<input class="input font-mono" bind:value={uri} placeholder="mongodb+srv://user:password@cluster0.example.net/app" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
            {#if uri.trim() && !isMongoUri(uri)}<p class="text-[12px] text-amber-700 dark:text-amber-400">The string must start with mongodb:// or mongodb+srv://.</p>{/if}
            <p class="flex items-center gap-1.5 text-[12px] text-faint"><KeyRound size={12} class="shrink-0" /> A password in the string is saved encrypted in sandb's data folder; the connections file keeps the string without it.</p>
          {:else}
            <div class="grid grid-cols-[1fr_96px] gap-2">
              <label class="block">{@render field('Host')}<input class="input font-mono" bind:value={host} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
              <label class="block">{@render field('Port')}<input class="input font-mono tabular-nums" type="number" bind:value={port} /></label>
            </div>
            <label class="block">{@render field('Database')}<input class="input font-mono" bind:value={database} placeholder={mongo ? 'optional' : 'app'} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
            <div class="grid grid-cols-2 gap-2">
              <label class="block">{@render field('User')}<input class="input font-mono" bind:value={user} placeholder={mongo ? 'optional' : undefined} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
              <label class="block">{@render field('Password')}<input class="input font-mono" type="password" bind:value={password} oninput={() => (passwordDirty = true)} placeholder={conn && !passwordDirty && api.kind !== 'mock' ? 'Saved · type to replace' : '••••••••'} /></label>
            </div>
            {#if mongo}
              <div class="grid grid-cols-2 gap-2">
                <label class="block">{@render field('Auth source')}<input class="input font-mono" bind:value={authSource} placeholder="optional, e.g. admin" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
                <!-- MongoDB has no Prefer mode. -->
                <label class="block">{@render field('TLS')}
                  <select class="input font-mono" bind:value={ssl}>
                    <option value="disable">Disable</option>
                    <option value="require">Require</option>
                  </select>
                </label>
              </div>
            {:else}
              <label class="block">{@render field('SSL mode')}
                <select class="input font-mono" bind:value={ssl}>
                  <option value="disable">Disable</option>
                  <option value="prefer">Prefer</option>
                  <option value="require">Require</option>
                </select>
              </label>
            {/if}
            {#if needsPassword}<p class="text-[12px] text-amber-700 dark:text-amber-400">Enter the password to test changed connection settings.</p>{/if}
            <p class="flex items-center gap-1.5 text-[12px] text-faint"><KeyRound size={12} /> The password is saved encrypted in sandb's data folder, not in the connections file.</p>
          {/if}
        {/if}

        <div class="flex items-center gap-3 border-t border-line pt-4 {readOnly ? 'opacity-60' : ''}">
          <div class="flex-1">
            <div class="font-medium">AI write access</div>
            <div class="text-[12.5px] text-muted">{readOnly ? 'Off while the connection is read-only.' : 'Off: the assistant can only read. On: it can propose writes, and you approve each one.'}</div>
          </div>
          <button
            role="switch"
            aria-checked={aiWrite}
            aria-label="AI write access"
            class="relative h-5 w-9 shrink-0 rounded-full transition disabled:cursor-not-allowed {aiWrite ? 'bg-amber-500' : 'bg-stone-500'}"
            disabled={readOnly}
            title={readOnly ? 'Turn off read-only to allow AI writes' : undefined}
            onclick={toggleAi}
          >
            <span class="absolute top-0.5 left-0.5 size-4 rounded-full bg-white shadow transition {aiWrite ? 'translate-x-4' : ''}"></span>
          </button>
        </div>
      </div>

      {#if (test === 'error' && testMsg) || saveError}
        <div class="mx-5 mb-3 space-y-1 rounded-lg bg-red-500/8 px-3 py-2 text-[12.5px] text-red-700 dark:text-red-300" role="alert">
          {#if test === 'error' && testMsg}<p class="break-words"><span class="sr-only">Error: </span><b class="font-semibold">Test:</b> {testMsg}</p>{/if}
          {#if saveError}<p class="break-words"><span class="sr-only">Error: </span><b class="font-semibold">Save failed:</b> {saveError}</p>{/if}
        </div>
      {/if}

      <footer class="dialog-foot border-t border-line py-3">
        <button class="btn btn-secondary" onclick={runTest} disabled={test === 'testing' || saving || !canSave}>Test connection</button>
        <span class="min-w-0 flex-1 truncate text-[12px]">
          {#if test === 'testing'}
            <span class="flex items-center gap-1.5 text-muted"><LoaderCircle size={13} class="animate-spin" /> Connecting…</span>
          {:else if test === 'ok'}
            <span class="flex items-center gap-1.5" title={testMsg}><span class="size-1.5 shrink-0 rounded-full bg-emerald-500"></span><span class="sr-only">Succeeded:</span><span class="truncate text-muted">{testMsg}</span></span>
          {:else if test === 'error'}
            <span class="flex items-center gap-1.5" title={testMsg}><span class="size-1.5 shrink-0 rounded-full bg-red-500"></span><span class="sr-only">Error:</span><span class="truncate text-red-700 dark:text-red-400">Connection failed</span></span>
          {/if}
        </span>
        <button class="btn btn-ghost" onclick={close}>Cancel</button>
        <button class="btn btn-primary" onclick={save} disabled={!canSave || saving}>
          {#if saving}<LoaderCircle size={13} class="animate-spin" />{/if} Save
        </button>
      </footer>
    </div>
  </div>
{/if}

<SafeguardDialog bind:target={guard} />
