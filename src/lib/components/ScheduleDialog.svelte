<script lang="ts">
  import { untrack } from 'svelte';
  import { CalendarClock, BookOpen } from '@lucide/svelte';
  import type { Cadence } from '../cadence';
  import { cadenceError, nextRuns, formatWhen } from '../cadence';
  import { catalog, loadDatabases } from '../catalog.svelte';
  import { canWrite, connOf, db, defaultDb, type Conn, type Engine } from '../connections.svelte';
  import { SKILLS } from '../api/generated/constants';
  import { saved, suggestName } from '../saved.svelte';
  import type { Notify, PromptTask, QueryTask, ScheduleInput, Task } from '../schedules.svelte';
  import { appliesTo, skillTitle } from '../skills';
  import Modal from './Modal.svelte';
  import ChatScope from './ChatScope.svelte';
  import ProdAck from './ProdAck.svelte';
  import Select from './ui/Select.svelte';
  import Switch from './ui/Switch.svelte';

  const frequencies = [
    { value: 'minutes', label: 'Minutes' },
    { value: 'hours', label: 'Hours' },
    { value: 'daily', label: 'Daily' },
    { value: 'weekly', label: 'Weekly' },
  ] as const;
  const days = [[1, 'Monday', 'M'], [2, 'Tuesday', 'T'], [3, 'Wednesday', 'W'], [4, 'Thursday', 'T'], [5, 'Friday', 'F'], [6, 'Saturday', 'S'], [0, 'Sunday', 'S']] as const;
  const notifications = [
    { value: 'always', label: 'Always' },
    { value: 'failure', label: 'Only when it fails' },
    { value: 'never', label: 'Never' },
  ] as const;

  let { initial, editing, answer }: { initial: ScheduleInput; editing: boolean; answer: (r: ScheduleInput | null) => void } = $props();

  // Each task kind keeps its own draft, so switching the type never loses input.
  let name = $state(untrack(() => initial.name));
  let kind = $state(untrack(() => initial.task.kind));
  let qd = $state<QueryTask>(
    untrack(() => (initial.task.kind === 'query' ? $state.snapshot(initial.task) : { kind: 'query', connId: db.connections[0]?.id ?? '', database: '', sql: '', allowWrites: false, prodAck: false })),
  );
  let pd = $state<PromptTask>(untrack(() => (initial.task.kind === 'prompt' ? $state.snapshot(initial.task) : { kind: 'prompt', sources: [], prompt: '', skills: [] })));
  let cad = $state<Cadence>(untrack(() => $state.snapshot(initial.cadence)));
  let notify = $state<Notify>(untrack(() => initial.notify));
  let enabled = $state(untrack(() => initial.enabled));
  /** The production tick; `qd.prodAck` records it on save only. */
  let ack = $state(false);

  const qconn = $derived<Conn | undefined>(db.connections.find((c) => c.id === qd.connId));
  const sqlite = $derived(qconn?.engine === 'sqlite');
  const prod = $derived(qconn?.env === 'production');
  const skillEngines = $derived<Engine[]>(pd.sources.map((s) => connOf(s)).filter((c): c is Conn => !!c).map((c) => c.engine));
  /** Skills that fit the chosen databases, plus picked ones that no longer fit so they can be removed. */
  const skillList = $derived(SKILLS.filter((s) => appliesTo(s.name, skillEngines) || pd.skills.includes(s.name)));

  /** Missing fields are not errors until the first Save; a bad schedule value shows at once. */
  let tried = $state(false);

  const problems = $derived.by(() => {
    const e: Record<string, string> = {};
    if (kind === 'query') {
      if (!qconn) e.conn = 'Pick a connection.';
      if (qconn && !sqlite && qconn.engine === 'mongo' && !qd.database.trim()) e.db = 'MongoDB needs a database.';
      if (!qd.sql.trim()) e.sql = qconn?.engine === 'mongo' ? 'The mongosh script to run is missing.' : 'The query to run is missing.';
      if (prod && qd.allowWrites && !ack) e.ack = 'Tick the box to confirm scheduled writes on production.';
    } else {
      if (!pd.sources.length) e.sources = 'Pick at least one database.';
      if (!pd.prompt.trim()) e.prompt = 'The prompt to send is missing.';
    }
    const c = cadenceError(cad);
    if (c) e.cadence = c;
    return e;
  });
  const valid = $derived(!Object.keys(problems).length);
  const err = $derived<Record<string, string>>(tried ? problems : problems.cadence ? { cadence: problems.cadence } : {});

  // The database list follows the picked connection; SQLite has no list to load.
  $effect(() => {
    const id = qd.connId;
    if (id && !sqlite) untrack(() => void loadDatabases(id));
  });

  /** The saved query fills the target and the text; an empty name takes its name too. */
  function pickSaved(id: string) {
    const q = saved.list.find((x) => x.id === id);
    if (!q) return;
    qd.connId = q.connId;
    qd.database = q.database;
    qd.sql = q.sql;
    if (!name.trim()) name = q.name;
    if (qconn && !canWrite(qconn)) qd.allowWrites = false;
    ack = false;
  }

  function pickConn(id: string) {
    qd.connId = id;
    const c = db.connections.find((x) => x.id === id);
    if (!c) return;
    qd.database = defaultDb(c);
    if (!canWrite(c)) qd.allowWrites = false;
    ack = false;
  }

  function setKind(k: QueryTask['kind'] | PromptTask['kind']) {
    kind = k;
    if (k === 'query' && qd.allowWrites && qconn && !canWrite(qconn)) qd.allowWrites = false;
  }

  function setCadence(k: Cadence['kind']) {
    if (cad.kind === k) return;
    cad = k === 'minutes' ? { kind: 'minutes', every: 15 } : k === 'hours' ? { kind: 'hours', every: 1 } : k === 'daily' ? { kind: 'daily', at: '09:00' } : { kind: 'weekly', days: [1, 2, 3, 4, 5], at: '09:00' };
  }

  function toggleDay(day: number) {
    if (cad.kind !== 'weekly') return;
    cad.days = cad.days.includes(day) ? cad.days.filter((d) => d !== day) : [...cad.days, day].sort((a, b) => a - b);
  }

  /** Writes through the discriminant: handlers never touch a member field directly. */
  function setEvery(n: number) {
    if (cad.kind === 'minutes' || cad.kind === 'hours') cad.every = n;
  }

  function setAt(v: string) {
    if (cad.kind === 'daily' || cad.kind === 'weekly') cad.at = v;
  }

  function toggleSkill(skill: string) {
    pd.skills = pd.skills.includes(skill) ? pd.skills.filter((n) => n !== skill) : [...pd.skills, skill];
  }

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!valid) {
      tried = true;
      return;
    }
    const task: Task =
      kind === 'query'
        ? { ...qd, database: qd.database.trim(), sql: qd.sql.trim(), prodAck: !!qconn && qconn.env === 'production' && qd.allowWrites && ack }
        : { ...pd, prompt: pd.prompt.trim() };
    answer(
      $state.snapshot({
        name: name.trim() || (task.kind === 'query' ? suggestName(task.sql) : task.prompt.replace(/\s+/g, ' ').slice(0, 40)),
        enabled,
        task,
        cadence: cad,
        notify,
      }),
    );
  }
</script>

<Modal
  title={editing ? 'Edit schedule' : 'New schedule'}
  width="max-w-xl"
  class="flex max-h-[calc(100dvh-2rem)] flex-col [&>form]:flex [&>form]:min-h-0 [&>form]:flex-col [&>form>header]:shrink-0"
  bodyClass="min-h-0 overflow-y-auto"
  footClass="shrink-0"
  onsubmit={submit}
  onclose={() => answer(null)}>
  {#snippet head()}<CalendarClock size={15} class="text-accent" />{/snippet}

  <div class="space-y-3.5">
    <div class="grid gap-3 sm:grid-cols-[1fr_auto] sm:items-end">
      <label class="block">
        <span class="label mb-1.5 block">Name</span>
        <input class="input input-sm" data-autofocus bind:value={name} placeholder={kind === 'query' ? 'Named after the table when empty' : 'First words of the prompt when empty'} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" />
      </label>
      <div>
        <span class="label mb-1.5 block">Type</span>
        <div class="seg">
          <button type="button" class="seg-item {kind === 'query' ? 'seg-on' : ''}" aria-pressed={kind === 'query'} onclick={() => setKind('query')}>Query</button>
          <button type="button" class="seg-item {kind === 'prompt' ? 'seg-on' : ''}" aria-pressed={kind === 'prompt'} onclick={() => setKind('prompt')}>AI prompt</button>
        </div>
      </div>
    </div>

    {#if kind === 'query'}
      <!-- Query task -->
      <div class="space-y-3 rounded-lg border border-line p-3">
        {#if saved.list.length}
          <label class="block">
            <span class="label mb-1.5 block">Load a saved query</span>
            <select class="input input-sm select-native" value="" onchange={(e) => { pickSaved(e.currentTarget.value); e.currentTarget.value = ''; }}>
              <option value="" disabled>Pick one to fill the fields below…</option>
              {#each saved.list as q (q.id)}
                <option value={q.id}>{q.name}</option>
              {/each}
            </select>
          </label>
        {/if}
        <div class="grid gap-3 sm:grid-cols-2">
          <label class="block">
            <span class="label mb-1.5 block">Connection</span>
            <select class="input input-sm select-native" value={qd.connId} onchange={(e) => pickConn(e.currentTarget.value)} aria-invalid={!!err.conn}>
              <option value="" disabled>Pick a connection…</option>
              {#each db.connections as c (c.id)}
                <option value={c.id}>{c.name} · {c.env}</option>
              {/each}
            </select>
          </label>
          {#if qconn && !sqlite}
            {@const databases = catalog.databases[qconn.id]}
            <label class="block">
              <span class="label mb-1.5 block">Database</span>
              {#if !databases || databases.status === 'loading'}
                <select class="input input-sm select-native" disabled><option>Loading databases…</option></select>
              {:else if databases.status === 'ok' && databases.items.length}
                <select class="input input-sm select-native" bind:value={qd.database} aria-invalid={!!err.db}>
                  <option value="">Pick a database…</option>
                  {#if qd.database && !databases.items.includes(qd.database)}
                    <option value={qd.database}>{qd.database}</option>
                  {/if}
                  {#each databases.items as n (n)}
                    <option value={n}>{n}</option>
                  {/each}
                </select>
              {:else}
                <input class="input input-sm font-mono" bind:value={qd.database} placeholder={defaultDb(qconn) || 'e.g. app'} autocapitalize="off" autocorrect="off" spellcheck="false" autocomplete="off" aria-invalid={!!err.db} />
              {/if}
            </label>
          {/if}
        </div>
        {#if err.conn || err.db}
          <p class="-mt-1.5 text-sm text-danger" role="alert">{err.conn ?? err.db}</p>
        {/if}
        <label class="block">
          <span class="label mb-1.5 block">{qconn?.engine === 'mongo' ? 'mongosh' : qconn?.engine === 'redis' ? 'Redis command' : 'SQL'}</span>
          <textarea class="input h-auto py-2 font-mono text-sm leading-relaxed" rows="6" bind:value={qd.sql} placeholder={qconn?.engine === 'mongo' ? 'db.orders.countDocuments({})' : qconn?.engine === 'redis' ? 'DBSIZE' : 'SELECT * FROM orders'} spellcheck="false" autocapitalize="off"></textarea>
        </label>
        {#if err.sql}
          <p class="-mt-1.5 text-sm text-danger" role="alert">{err.sql}</p>
        {/if}
        <div>
          <span class="label mb-1.5 block">Permission</span>
          <div class="seg">
            <button type="button" class="seg-item {!qd.allowWrites ? 'seg-on' : ''}" aria-pressed={!qd.allowWrites} onclick={() => (qd.allowWrites = false)}>Read-only</button>
            <button
              type="button"
              class="seg-item {qd.allowWrites ? 'seg-on' : ''} disabled:cursor-not-allowed disabled:opacity-50"
              aria-pressed={qd.allowWrites}
              disabled={!!qconn && !canWrite(qconn)}
              title={qconn && !canWrite(qconn) ? 'This connection is read-only.' : undefined}
              onclick={() => (qd.allowWrites = true)}>Allow writes</button>
          </div>
          <p class="mt-1.5 text-sm leading-relaxed text-faint">
            {#if qd.allowWrites}Runs every statement and commits at once. Nobody is asked first.
            {:else if qconn && !canWrite(qconn)}One read statement. It runs in a read-only session, up to 100 rows and 5 minutes. This connection is read-only.
            {:else}One read statement. It runs in a read-only session, up to 100 rows and 5 minutes.{/if}
          </p>
        </div>
        {#if prod && qd.allowWrites}
          <ProdAck bind:checked={ack}>Scheduled writes run on it without asking.</ProdAck>
          {#if err.ack}
            <p class="mt-1 text-sm text-danger" role="alert">{err.ack}</p>
          {/if}
        {/if}
      </div>
    {:else}
      <!-- AI prompt task -->
      <div class="space-y-3 rounded-lg border border-line p-3">
        <div>
          <span class="label mb-1.5 block">Databases</span>
          <ChatScope sources={pd.sources} onchange={(list) => (pd.sources = list)} placement="down" note="" />
          {#if err.sources}
            <p class="mt-0.5 text-sm text-danger" role="alert">{err.sources}</p>
          {/if}
        </div>
        <label class="block">
          <span class="label mb-1.5 block">Prompt</span>
          <textarea class="input h-auto py-2 leading-relaxed" rows="4" bind:value={pd.prompt} placeholder="e.g. How many orders came in this week?" autocapitalize="off" spellcheck="false"></textarea>
        </label>
        {#if err.prompt}
          <p class="-mt-1.5 text-sm text-danger" role="alert">{err.prompt}</p>
        {/if}
        {#if skillList.length}
          <div>
            <span class="label mb-1.5 block">Skills</span>
            <div class="flex flex-wrap items-center gap-1">
              {#each skillList as s (s.name)}
                {@const applies = appliesTo(s.name, skillEngines)}
                {@const on = pd.skills.includes(s.name)}
                <button
                  type="button"
                  class="chip min-w-0"
                  class:chip-on={on}
                  title={applies ? s.description : 'Does not apply to the chosen databases. Click to remove.'}
                  aria-pressed={on}
                  onclick={() => toggleSkill(s.name)}>
                  <BookOpen size={12} class="shrink-0 {on ? 'text-accent' : 'text-faint'}" />
                  <span class="min-w-0 truncate">{skillTitle(s.name)}</span>
                </button>
              {/each}
            </div>
          </div>
        {/if}
        <p class="text-sm leading-relaxed text-faint">The assistant only reads in a scheduled run. Each run starts a new chat in the chat history.</p>
      </div>
    {/if}

    <!-- Cadence -->
    <div>
      <label for="schedule-frequency" class="label mb-1.5 block">Schedule</label>
      <Select id="schedule-frequency" aria-label="Schedule frequency" size="sm" class="w-36" value={cad.kind} options={frequencies} onchange={setCadence} />
      <div class="mt-2.5 flex flex-wrap items-center gap-x-2 gap-y-1.5 text-sm text-muted">
        {#if cad.kind === 'minutes'}
          Every
          <input class="input input-sm w-16 font-mono tabular-nums" type="number" min="1" max="59" value={cad.every} oninput={(e) => setEvery(e.currentTarget.valueAsNumber)} aria-label="Every this many minutes" />
          minutes
        {:else if cad.kind === 'hours'}
          Every
          <input class="input input-sm w-16 font-mono tabular-nums" type="number" min="1" max="24" value={cad.every} oninput={(e) => setEvery(e.currentTarget.valueAsNumber)} aria-label="Every this many hours" />
          {cad.every === 1 ? 'hour' : 'hours'}
        {:else if cad.kind === 'daily'}
          Every day at
          <input class="input input-sm w-28 font-mono" type="time" value={cad.at} oninput={(e) => setAt(e.currentTarget.value)} aria-label="Time of day" />
        {:else}
          <div class="flex flex-wrap items-center gap-1" role="group" aria-label="Days of the week">
            {#each days as [day, full, letter] (day)}
              <button
                type="button"
                class="chip w-7 justify-center font-medium"
                class:chip-on={cad.days.includes(day)}
                aria-label={full}
                aria-pressed={cad.days.includes(day)}
                onclick={() => toggleDay(day)}>{letter}</button>
            {/each}
          </div>
          at
          <input class="input input-sm w-28 font-mono" type="time" value={cad.at} oninput={(e) => setAt(e.currentTarget.value)} aria-label="Time of day" />
        {/if}
      </div>
      {#if err.cadence}
        <p class="mt-1.5 text-sm text-danger" role="alert">{err.cadence}</p>
      {:else}
        <p class="mt-1.5 text-sm text-faint">Next: {nextRuns(cad, Date.now(), 3).map((t) => formatWhen(t)).join(' · ')}</p>
      {/if}
    </div>

    <div class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
      <div>
        <label for="schedule-notify" class="label mb-1.5 block">Notify</label>
        <Select id="schedule-notify" aria-label="Notify" aria-describedby="schedule-notify-help" size="sm" bind:value={notify} options={notifications} />
        <p id="schedule-notify-help" class="mt-1.5 text-sm text-faint">A toast while sandb is in front, a system notification when it is not.</p>
      </div>
      <div>
        <span class="label mb-1.5 block">Status</span>
        <div class="flex h-7 items-center gap-2">
          <Switch bind:checked={enabled} aria-label="Schedule is active" />
          <span class="w-12 text-sm text-ink">{enabled ? 'Active' : 'Paused'}</span>
        </div>
      </div>
    </div>
  </div>

  {#snippet footer()}
    <button type="button" class="btn btn-sm btn-secondary" onclick={() => answer(null)}>Cancel</button>
    <button type="submit" class="btn btn-sm btn-primary" disabled={tried && !valid}>Save <kbd class="ml-0.5 font-sans font-normal opacity-80" aria-hidden="true">↵</kbd></button>
  {/snippet}
</Modal>
