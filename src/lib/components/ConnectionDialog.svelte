<script lang="ts">
  import { Database, LoaderCircle, KeyRound } from '@lucide/svelte';
  import { untrack } from 'svelte';
  import { db, defaultDb, engines, environmentLabel, sameTarget, type Conn, type Engine, type Env } from '../connections.svelte';
  import { active } from '../tabs.svelte';
  import { activeTab, setTabConn } from '../tabs.svelte';
  import { api, errorText, type Connection, type ConnectionInput, type MongoUriParts, type RedisMode, type RedisSettings, type RedisUriParts, type SslMode, type Topology } from '../api';
  import { ask } from '../ask.svelte';
  import Modal from './Modal.svelte';
  import SafeguardDialog from './SafeguardDialog.svelte';
  import EngineBadge from './ui/EngineBadge.svelte';
  import Select from './ui/Select.svelte';
  import Switch from './ui/Switch.svelte';

  // `conn` set: edit it, or, if its id is not in db.connections yet, save it as a new copy (duplicate).
  let {
    open = $bindable(false),
    conn = null,
    onsaved,
    onedited,
    canswitch,
    canupdate,
  }: {
    open?: boolean;
    conn?: Conn | null;
    onsaved: () => void;
    onedited?: (c: Conn, dbChanged: boolean) => void;
    canswitch: () => boolean | Promise<boolean>;
    canupdate: (c: Conn, dbChanged: boolean) => boolean | Promise<boolean>;
  } = $props();

  const redisDefaults: RedisSettings = { mode: 'standalone', sentinelMaster: '', sentinelAuth: false };
  const sentinelPort = 26379;

  let engine = $state<Engine>('pg');
  let name = $state('');
  let host = $state('localhost');
  let port = $state(5432);
  let database = $state('');
  let user = $state('postgres');
  let password = $state('');
  let ssl = $state<SslMode>('prefer');
  let file = $state('~/data/app.db');
  let label = $state('');
  let standardEnv = $state<Exclude<Env, 'production'>>('local');
  let protectedWrites = $state(false);
  const env: Env = $derived(protectedWrites ? 'production' : standardEnv);
  const envLabel = $derived(label.trim() === env ? '' : label.trim());
  let aiWrite = $state(false);
  let readOnly = $state(false);
  // MongoDB and standalone Redis accept a connection string (`uri`, sent as typed, password included).
  let mode = $state<'fields' | 'uri'>('fields');
  let uri = $state('');
  let authSource = $state('');
  let redis = $state<RedisSettings>({ ...redisDefaults });
  let test = $state<'idle' | 'testing' | 'ok' | 'error'>('idle');
  let testMsg = $state('');
  // Version and topology from the last successful test, valid only while the connection fields match `testedSig`.
  let tested: { sig: string; version: string; topology: Topology | null } | null = null;
  // Saved connections keep their stored password unless the user types a new one.
  let passwordDirty = $state(false);
  let saving = $state(false);
  let saveError = $state('');
  // A newer test or reset invalidates an in-flight test.
  let run = 0;
  // The backend's split of the typed string, without its password: the saved one is compared with it.
  let uriPreview = $state<{ engine: Engine; text: string; parts: MongoUriParts | RedisUriParts | null } | null>(null);

  const existing = $derived(conn ? db.connections.find((x) => x.id === conn.id) : undefined);
  const mongo = $derived(engine === 'mongo');
  const redisEngine = $derived(engine === 'redis');
  const multiHost = $derived(redisEngine && redis.mode !== 'standalone');
  const sentinel = $derived(redisEngine && redis.mode === 'sentinel');
  const useUri = $derived((mongo || (redisEngine && !multiHost)) && mode === 'uri');

  const validUri = $derived((redisEngine ? /^rediss?:\/\//i : /^mongodb(\+srv)?:\/\//i).test(uri.trim()));
  const currentUriPreview = $derived(uriPreview?.engine === engine && uriPreview.text === uri.trim() ? uriPreview : null);

  $effect(() => {
    const text = open && useUri && validUri ? uri.trim() : '';
    if (!text) {
      uriPreview = null;
      return;
    }
    const atEngine = engine;
    const parseUri = redisEngine ? api.parseRedisUri : api.parseMongoUri;
    let gone = false;
    void (async () => {
      const parts = await parseUri({ uri: text }).catch(() => null);
      if (!gone) uriPreview = { engine: atEngine, text, parts };
    })();
    return () => {
      gone = true;
    };
  });

  $effect(() => {
    // Reading `conn` re-fills when ⌘K "New connection" replaces an open edit.
    if (open) {
      conn;
      untrack(() => fill(conn));
    }
  });

  // SafeguardDialog answers true; the connection changes only when the user saves.
  async function toggleAi(next: boolean) {
    if (!next) return (aiWrite = false);
    if (readOnly) return;
    if (await ask<boolean>(SafeguardDialog, { conn: { engine, name: name.trim() || 'new connection', env, envLabel, readOnly } })) aiWrite = true;
  }

  function fill(c: Conn | null) {
    if (!c) {
      // A new connection always starts from a clean draft, never from the last edited or saved one.
      setEngine('pg');
      name = database = password = '';
      host = 'localhost';
      ssl = 'prefer';
      file = '~/data/app.db';
      label = '';
      standardEnv = 'local';
      protectedWrites = false;
      aiWrite = false;
      readOnly = false;
      mode = 'fields';
      uri = authSource = '';
      resetStatus();
      return;
    }
    engine = c.engine;
    redis = { ...redisDefaults, ...c.redis };
    name = c.name;
    database = c.database;
    host = c.host || 'localhost';
    port = c.port || (sentinel ? sentinelPort : engines[c.engine].port);
    user = c.user;
    // The stored password is never sent to the UI.
    password = '';
    ssl = c.ssl;
    file = c.file || '~/data/app.db';
    label = environmentLabel(c);
    standardEnv = c.env === 'staging' ? 'staging' : 'local';
    protectedWrites = c.env === 'production';
    aiWrite = c.aiWrite;
    readOnly = c.readOnly;
    // The saved string has no password; the stored one is kept unless the user types one into it.
    uri = c.uri;
    mode = c.uri ? 'uri' : 'fields';
    if (multiHost) mode = 'fields';
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

  // A read-only connection refuses AI writes too, so turning it on also turns AI write access off.
  function toggleReadOnly(next: boolean) {
    readOnly = next;
    if (readOnly) aiWrite = false;
  }

  const engineList: Engine[] = ['pg', 'mysql', 'sqlite', 'mongo', 'redis'];
  const sslModes = [
    { value: 'disable', label: 'Disable' },
    { value: 'prefer', label: 'Prefer' },
    { value: 'require', label: 'Require' },
  ] as const;
  // MongoDB and Redis have no Prefer mode.
  const tlsModes = sslModes.filter(({ value }) => value !== 'prefer');
  const connectMethods = [
    { value: 'fields', label: 'Fields' },
    { value: 'uri', label: 'Connection string' },
  ] as const;
  const fieldMethods = [connectMethods[0], { ...connectMethods[1], disabled: true }];
  const deployments = [
    { value: 'standalone', label: 'Standalone' },
    { value: 'cluster', label: 'Cluster' },
    { value: 'sentinel', label: 'Sentinel' },
  ] as const;
  // MongoDB can omit the database. Redis uses 0 by default.
  const canSave = $derived.by(() => {
    if (!name.trim()) return false;
    if (engine === 'sqlite') return !!file.trim();
    // Redis waits for its preview so Test and Save compare the redacted target, not stale fields.
    if (useUri) return validUri && (!redisEngine || currentUriPreview !== null);
    return !!host.trim() && (mongo || redisEngine || !!database.trim()) && (!sentinel || !!redis.sentinelMaster.trim());
  });

  function setEngine(e: Engine) {
    // MongoDB and Redis start with TLS disabled.
    if (e === 'mongo' || e === 'redis') ssl = 'disable';
    else if (mongo || redisEngine) ssl = 'prefer';
    if (e === 'redis' || redisEngine) {
      mode = 'fields';
      uri = '';
    }
    if (e === 'redis') redis = { ...redisDefaults };
    engine = e;
    port = engines[e].port;
    user = e === 'pg' ? 'postgres' : e === 'mysql' ? 'root' : '';
    test = 'idle';
  }

  function setDeployment(next: RedisMode) {
    if (next === 'sentinel' && port === engines.redis.port) port = sentinelPort;
    else if (sentinel && port === sentinelPort) port = engines.redis.port;
    redis.mode = next;
    if (next !== 'standalone') mode = 'fields';
  }

  function redisSettings(): RedisSettings | null {
    if (!redisEngine) return null;
    if (!sentinel) return { ...redisDefaults, mode: redis.mode };
    return { mode: 'sentinel', sentinelMaster: redis.sentinelMaster.trim(), sentinelAuth: redis.sentinelAuth };
  }

  function redisDatabase(): string {
    return redis.mode === 'cluster' ? '0' : (database.trim() || '0').replace(/^0+(?=\d+$)/, '');
  }

  function input(): ConnectionInput & Pick<Connection, 'uri' | 'authSource'> {
    return { name: name.trim(), engine, env, envLabel, host: host.trim(), port: Number(port) || 0, database: redisEngine ? redisDatabase() : database.trim(), user: user.trim(), ssl, file: file.trim(), aiWrite, readOnly, uri: useUri ? uri.trim() : '', authSource: mongo && !useUri ? authSource.trim() : '', redis: redisSettings() };
  }

  // Fields that decide where the connection goes; a test result is stale once any of them changes.
  const sig = $derived(JSON.stringify([engine, useUri, host, port, database, user, ssl, file, password, uri, authSource, redisSettings()]));
  $effect(() => {
    sig;
    untrack(() => (test = 'idle'));
  });

  // null = keep the stored secret (edit) or the source's secret (duplicate).
  function passwordArg(): string | null {
    if (engine === 'sqlite') return existing ? null : '';
    // A password typed into the string wins over this argument; without one the stored password is kept.
    if (useUri) return null;
    if (conn && !passwordDirty) return null;
    return password;
  }

  // Reuse a stored password only for the same target; SQLite has no password to reuse.
  const secretId = () => (conn && !passwordDirty && engine !== 'sqlite' && !targetChanged() ? (existing ? conn.id : (conn.copyOf ?? null)) : null);

  /** Where the fields currently point, in the shape `sameTarget` compares (engine defaults included). */
  function typedTarget() {
    const base = { engine, host: host.trim(), port: Number(port) || (sentinel ? sentinelPort : engines[engine].port), user: user.trim(), file: engine === 'sqlite' ? file.trim() : '', uri: '', redis: redisSettings(), database: redisEngine ? redisDatabase() : database.trim() };
    if (!useUri) return base;
    // A typed string decides the target, and its password is not part of the saved one.
    if (currentUriPreview?.parts) {
      const { host, port, user, redacted, database } = currentUriPreview.parts;
      return { ...base, host, port, user, uri: redacted, database };
    }
    if (conn && uri.trim() === conn.uri) {
      const { host, port, user, uri, database } = conn;
      return { ...base, host, port, user, uri, database };
    }
    return base;
  }

  // The backend refuses to send a stored password to another target; say so before the round trip.
  function targetChanged(): boolean {
    const src = conn && db.connections.find((x) => x.id === (existing ? conn.id : conn.copyOf));
    return !!src && !sameTarget(src, typedTarget());
  }
  const needsPassword = $derived(api.capabilities.secretStorage !== 'none' && engine !== 'sqlite' && !useUri && !!conn && !passwordDirty && targetChanged());
  const failed = $derived(saveError ? `Save failed: ${saveError}` : null);

  async function runTest() {
    if (needsPassword) {
      testMsg = 'Enter the password to test changed connection settings';
      test = 'error';
      return;
    }
    test = 'testing';
    testMsg = '';
    const at = sig;
    const mine = ++run;
    try {
      const r = await api.testConnection({ input: input(), password: passwordArg(), id: secretId() });
      if (mine !== run || at !== sig || test !== 'testing') return;
      tested = { sig: at, version: r.serverVersion, topology: r.topology ?? null };
      testMsg = `Connected · ${r.serverVersion}${r.topology ? ` · ${r.topology.replace('_', ' ')}` : ''} · ${r.latencyMs} ms`;
      test = 'ok';
    } catch (e) {
      if (mine !== run || at !== sig || test !== 'testing') return;
      testMsg = errorText(e);
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

  /** The saved connection joins the list and the active tab; both create paths end here. */
  function adopt(saved: Conn) {
    db.connections.push(saved);
    const t = activeTab();
    if (t) setTabConn(t, saved);
    close();
    onsaved();
  }

  async function save() {
    if (saving) return;
    if (conn) return saveConn(conn);
    if (!(await canswitch())) return;
    saving = true;
    saveError = '';
    let created: Conn;
    try {
      created = await api.createConnection({ input: payload(), password: passwordArg() });
    } catch (e) {
      saveError = errorText(e);
      saving = false;
      return;
    }
    adopt(created);
  }

  async function saveConn(c: Conn) {
    const cur = existing;
    if (!cur) {
      if (!(await canswitch())) return;
      saving = true;
      saveError = '';
      let saved: Conn;
      try {
        const src = c.copyOf;
        saved = src ? await api.duplicateConnection({ id: src, input: payload(), password: passwordArg() }) : await api.createConnection({ input: payload(), password: passwordArg() });
      } catch (e) {
        saveError = errorText(e);
        saving = false;
        return;
      }
      adopt(saved);
      return;
    }
    const next = payload();
    const target = typedTarget();
    const nextDb = redisEngine ? target.database : defaultDb(next);
    // Only a change of target or database touches the open result; name, env and AI access do not.
    const dbChanged = cur.database !== nextDb || !sameTarget(cur, target);
    if (dbChanged && cur.id === active().id && !(await canswitch())) return;
    if (!(await canupdate(cur, dbChanged))) return;
    saving = true;
    saveError = '';
    try {
      const saved = await api.updateConnection({ id: cur.id, input: next, password: passwordArg() });
      Object.assign(cur, saved);
    } catch (e) {
      saveError = errorText(e);
      saving = false;
      return;
    }
    close();
    onedited?.(cur, dbChanged);
  }
</script>

{#snippet field(text: string)}
  <span class="label mb-1.5 block">{text}</span>
{/snippet}

{#if open}
  <Modal
    title={!conn ? 'New connection' : existing ? 'Edit connection' : 'Duplicate connection'}
    width="max-w-lg"
    class="flex max-h-[calc(100dvh-2rem)] flex-col [&>header]:shrink-0"
    bodyClass="min-h-0 flex-1 space-y-4 overflow-y-auto pb-5"
    footClass="min-h-12 shrink-0 border-t border-line py-3"
    closable
    busy={saving}
    error={failed}
    onclose={close}
  >
        {#snippet head()}<Database size={15} class="text-accent" />{/snippet}
        <p class="-mt-2 text-sm text-muted">{conn ? `${engines[engine].label} · the engine of a saved connection cannot change.` : 'Connect to a PostgreSQL, MySQL, SQLite, MongoDB or Redis database.'}</p>

        <div class="grid grid-cols-3 gap-2">
          {#each engineList as e}
            <button
              class="focus-ring flex flex-col items-start gap-1 rounded-lg border px-3 py-2.5 text-left motion-safe:transition-colors disabled:cursor-not-allowed {engine === e ? 'border-accent/70 bg-accent/10 ring-3 ring-accent/12' : conn ? 'border-line opacity-40' : 'border-line hover:bg-hover'}"
              disabled={!!conn}
              title={conn && engine !== e ? 'Engine cannot be changed' : undefined}
              onclick={() => setEngine(e)}
            >
              <EngineBadge engine={e} />
              <span class="text-base font-medium">{engines[e].label}</span>
            </button>
          {/each}
        </div>

        <label class="block">{@render field('Name')}<input class="input input-sm" data-autofocus bind:value={name} placeholder="e.g. billing-prod" autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>

        <!-- Above the target fields so Environment and Read-only stay in view on short windows; the body still scrolls. -->
        <div class="space-y-2">
          <label class="block">
            {@render field('Environment label')}
            <input class="input input-sm" bind:value={label} maxlength="48" placeholder={env} aria-describedby="env-label-hint" />
          </label>
          <p id="env-label-hint" class="text-xs text-muted">For example, Finance EU or QA. The label does not set permissions.</p>
          <div class="flex items-center gap-3 pt-1">
            <div class="flex-1">
              <div class="font-medium">Protect writes</div>
              <div class="text-sm text-muted">{mongo || redisEngine ? 'Ask before running editor writes.' : 'Use manual commit in SQL editor tabs.'} Confirm dangerous changes.</div>
            </div>
            <Switch checked={protectedWrites} aria-label="Protect writes" onchange={(next) => (protectedWrites = next)} />
          </div>
          <div class="flex items-center gap-3 pt-1">
            <div class="flex-1">
              <div class="font-medium">Read-only</div>
              <div class="text-sm text-muted">Blocks all writes, including your own queries</div>
            </div>
            <Switch checked={readOnly} aria-label="Read-only connection" onchange={toggleReadOnly} />
          </div>
          {#if mongo && readOnly}<p class="text-sm text-warn">Use a database user with the read role for full protection.</p>{/if}
          {#if redisEngine && readOnly}<p class="text-sm text-warn">Redis has no read-only session. Use an ACL user without write commands (for example +@read) for full protection.</p>{/if}
        </div>

        {#if engine === 'sqlite'}
          <label class="block">{@render field('Database file')}<input class="input input-sm" bind:value={file} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
        {:else}
          {#if redisEngine}
            <div>
              <label for="redis-deployment" class="label mb-1.5 block">Deployment</label>
              <Select id="redis-deployment" aria-label="Deployment" size="sm" value={redis.mode} options={deployments} onchange={setDeployment} />
            </div>
          {/if}
          {#if mongo || redisEngine}
            <div>
              <label for="connection-auth" class="label mb-1.5 block">Connect with</label>
              <Select id="connection-auth" aria-label="Connect with" size="sm" bind:value={mode} options={multiHost ? fieldMethods : connectMethods} />
              {#if multiHost}<p class="mt-1.5 text-xs text-faint">Connection strings are available only for Standalone.</p>{/if}
            </div>
          {/if}
          {#if useUri}
            <label class="block">{@render field('Connection string')}<input class="input input-sm font-mono" bind:value={uri} placeholder={redisEngine ? 'redis://user:password@localhost:6379/0' : 'mongodb+srv://user:password@cluster0.example.net/app'} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
            {#if uri.trim() && !validUri}<p class="text-sm text-warn">{redisEngine ? 'The string must start with redis:// or rediss://.' : 'The string must start with mongodb:// or mongodb+srv://.'}</p>{/if}
            <p class="flex items-center gap-1.5 text-xs text-faint"><KeyRound size={12} class="shrink-0" /> A password in the string is saved encrypted in sandb's data folder; the connections file keeps the string without it.</p>
          {:else}
            <div class="grid grid-cols-[1fr_96px] gap-2">
              <label class="block">{@render field(multiHost ? 'Hosts' : 'Host')}<input class="input input-sm font-mono" bind:value={host} placeholder={multiHost ? '10.0.0.1:6379, 10.0.0.2:6379' : undefined} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
              <label class="block">{@render field('Port')}<input class="input input-sm font-mono tabular-nums" type="number" bind:value={port} /></label>
            </div>
            {#if sentinel}
              <label class="block">{@render field('Master name')}<input class="input input-sm" bind:value={redis.sentinelMaster} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
              <div class="flex items-center gap-3">
                <div class="flex-1 font-medium">Sentinels use the same user and password</div>
                <Switch checked={redis.sentinelAuth} aria-label="Sentinels use the same user and password" onchange={(next) => (redis.sentinelAuth = next)} />
              </div>
            {/if}
            {#if !redisEngine || redis.mode !== 'cluster'}
              <label class="block">{@render field('Database')}<input class="input input-sm" bind:value={database} placeholder={redisEngine ? '0' : mongo ? 'optional' : 'app'} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
            {/if}
            <div class="grid grid-cols-2 gap-2">
              <label class="block">{@render field('User')}<input class="input input-sm" bind:value={user} placeholder={redisEngine ? 'optional (ACL user)' : mongo ? 'optional' : undefined} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" /></label>
              <label class="block">{@render field('Password')}<input class="input input-sm" type="password" bind:value={password} oninput={() => (passwordDirty = true)} placeholder={conn && !passwordDirty && api.capabilities.secretStorage !== 'none' ? 'Saved · type to replace' : '••••••••'} /></label>
            </div>
            {#if mongo || redisEngine}
              <div class={mongo ? 'grid grid-cols-2 gap-2' : undefined}>
                {#if mongo}<label class="block">{@render field('Auth source')}<input class="input input-sm" bind:value={authSource} placeholder="optional, e.g. admin" autocapitalize="off" spellcheck="false" autocomplete="off" /></label>{/if}
                <div>
                  <label for="connection-tls" class="label mb-1.5 block">TLS</label>
                  <Select id="connection-tls" aria-label="TLS" size="sm" bind:value={ssl} options={tlsModes} />
                </div>
              </div>
            {:else}
              <div>
                <label for="connection-ssl" class="label mb-1.5 block">SSL mode</label>
                <Select id="connection-ssl" aria-label="SSL mode" size="sm" bind:value={ssl} options={sslModes} />
              </div>
            {/if}
            {#if needsPassword}<p class="text-sm text-warn">Enter the password to test changed connection settings.</p>{/if}
            <p class="flex items-center gap-1.5 text-xs text-faint"><KeyRound size={12} class="shrink-0" /> The password is saved encrypted in sandb's data folder, not in the connections file.</p>
          {/if}
        {/if}

        <div class="flex items-center gap-3 border-t border-line pt-4 {readOnly ? 'opacity-60' : ''}">
          <div class="flex-1">
            <div class="font-medium">AI write access</div>
            <div class="text-sm text-muted">{readOnly ? 'Off while the connection is read-only.' : 'Off: the assistant can only read. On: it can propose writes, and you approve each one.'}</div>
          </div>
          <Switch
            checked={aiWrite}
            aria-label="AI write access"
            tone="warn"
            disabled={readOnly}
            title={readOnly ? 'Turn off read-only to allow AI writes' : undefined}
            onchange={toggleAi}
          />
        </div>

    {#snippet footer()}
      <button class="btn btn-sm btn-secondary" onclick={runTest} disabled={test === 'testing' || saving || !canSave}>Test connection</button>
      <div class="min-w-0 flex-1 text-xs" role="status">
        {#if test === 'testing'}
          <span class="flex items-center gap-1.5 text-muted"><LoaderCircle size={12} class="shrink-0 motion-safe:animate-spin" /> Connecting…</span>
        {:else if test === 'ok' || test === 'error'}
          <div class="flex items-start gap-1.5 {test === 'ok' ? 'text-ok' : 'text-danger'}" title={testMsg}>
            <span class="mt-1.5 size-1.5 shrink-0 rounded-full bg-current"></span>
            <span class="min-w-0 max-h-16 overflow-y-auto break-words">
              {#if test === 'error'}<span class="sr-only">Connection failed: </span>{/if}{testMsg}
            </span>
          </div>
        {/if}
      </div>
      <button class="btn btn-sm btn-ghost" onclick={close}>Cancel</button>
      <button class="btn btn-sm btn-primary" onclick={save} disabled={!canSave || saving}>
        {#if saving}<LoaderCircle size={12} class="motion-safe:animate-spin" />{/if} Save
      </button>
    {/snippet}
  </Modal>
{/if}
