import { api, ApiError, type Cell, type ErrorCode } from './api';
import { ask } from './ask.svelte';
import { logQuery } from './audit.svelte';
import { DEFAULT_CADENCE, formatWhen, nextRun, type Cadence } from './cadence';
import { afterRun } from './catalog.svelte';
import { chat, runScheduledPrompt } from './chat.svelte';
import ScheduleDialog from './components/ScheduleDialog.svelte';
import { connOf, db, defaultDb, type Source } from './connections.svelte';
import { notify } from './notice.svelte';
import { osNotify } from './osNotify';
import { autosave, readStored } from './persist.svelte';
import { type SavedQuery } from './saved.svelte';
import { ui } from './state.svelte';
import { active } from './tabs.svelte';
import { classify } from './writeCheck';

export type Notify = 'always' | 'failure' | 'never';
export interface QueryTask {
  kind: 'query';
  connId: string;
  database: string;
  sql: string;
  allowWrites: boolean;
  /** The user confirmed scheduled writes on a production connection. */
  prodAck: boolean;
}
export interface PromptTask {
  kind: 'prompt';
  sources: Source[];
  prompt: string;
  skills: string[];
}
export type Task = QueryTask | PromptTask;
export type RunStatus = 'ok' | 'error' | 'blocked';
export type Trigger = 'schedule' | 'catch-up' | 'manual';
export interface ScheduleRun {
  id: string;
  trigger: Trigger;
  started: number;
  ended: number;
  status: RunStatus;
  /** One line: "42 rows", "100+ rows", "3 rows affected", "2 statements ran", the error text, or the reply's first line. */
  message: string;
  columns?: string[];
  rows?: Cell[][];
  truncated?: boolean; // query output
  chatId?: string;
  reply?: string; // prompt output
}
export interface Schedule {
  id: string;
  name: string;
  enabled: boolean;
  task: Task;
  cadence: Cadence;
  notify: Notify;
  created: number;
  updated: number;
  nextAt: number;
  /** Runs that ended after this are unseen. */
  seenAt: number;
  runs: ScheduleRun[]; // newest first
}
export type ScheduleInput = Pick<Schedule, 'name' | 'enabled' | 'task' | 'cadence' | 'notify'>;

const KEY = 'sandb.schedules.v1';
export const MAX_RUNS = 20; // history kept per schedule
export const RESULT_RUNS = 3; // newest runs that keep rows / reply text; older ones keep only status + message
export const STORED_ROWS = 100; // rows fetched and kept per query run
export const REPLY_CHARS = 4000; // reply text kept per prompt run
export const TICK_MS = 15_000;

/** Codes of a run the guard or the read-only rules refused: `blocked`, never `error`. */
const BLOCKED: Partial<Record<ErrorCode, true>> = { guard_rejected: true, write_blocked: true, read_only_violation: true, connection_read_only: true };

export const schedules = $state({
  list: readStored<Schedule[]>(KEY, [], { migrate: (d) => (Array.isArray(d) ? (d as Schedule[]) : []) }),
  selected: null as string | null,
  /** The one run in flight; `stop` cancels it where the path can be cancelled. */
  running: null as null | { id: string; started: number; stop: (() => void) | null },
  queue: [] as { id: string; trigger: Trigger }[],
});

autosave(KEY, () => schedules.list, { delay: 300, onError: (e) => console.warn('Schedules were not saved.', e) });

export const findSchedule = (id: string | null | undefined) => (id ? schedules.list.find((s) => s.id === id) : undefined);

export function createSchedule(input: ScheduleInput): Schedule {
  const now = Date.now();
  const s: Schedule = { ...input, id: crypto.randomUUID(), created: now, updated: now, seenAt: now, nextAt: nextRun(input.cadence, now), runs: [] };
  schedules.list.push(s);
  return s;
}

export function updateSchedule(id: string, input: ScheduleInput) {
  const s = findSchedule(id);
  if (!s) return;
  const now = Date.now();
  const cadenceChanged = JSON.stringify(s.cadence) !== JSON.stringify(input.cadence);
  const reEnabled = !s.enabled && input.enabled;
  Object.assign(s, input);
  s.updated = now;
  if (cadenceChanged || reEnabled) s.nextAt = nextRun(s.cadence, now);
}

/** Pausing and resuming has no catch-up: a re-enabled schedule waits for its next slot. */
export function setEnabled(id: string, on: boolean) {
  const s = findSchedule(id);
  if (!s || s.enabled === on) return;
  s.enabled = on;
  s.updated = Date.now();
  if (on) s.nextAt = nextRun(s.cadence, Date.now());
}

export function duplicateSchedule(id: string): Schedule | undefined {
  const s = findSchedule(id);
  if (!s) return;
  const now = Date.now();
  const copy: Schedule = {
    ...s,
    id: crypto.randomUUID(),
    name: `${s.name} copy`,
    enabled: false,
    task: $state.snapshot(s.task) as Task,
    cadence: $state.snapshot(s.cadence),
    created: now,
    updated: now,
    seenAt: now,
    nextAt: nextRun(s.cadence, now),
    runs: [],
  };
  schedules.list.push(copy);
  return copy;
}

export function deleteSchedule(id: string) {
  if (schedules.running?.id === id) stopRun();
  schedules.queue = schedules.queue.filter((e) => e.id !== id);
  schedules.list = schedules.list.filter((s) => s.id !== id);
  if (schedules.selected === id) schedules.selected = null;
}

export function openSchedule(id: string | null) {
  ui.page = 'scheduler';
  schedules.selected = id;
  const s = findSchedule(id);
  if (s) s.seenAt = Date.now();
}

export const unseen = (s: Schedule) => s.runs.filter((r) => r.ended > s.seenAt).length;

export function unseenFailed(): boolean {
  return schedules.list.some((s) => s.runs.some((r) => r.ended > s.seenAt && r.status !== 'ok'));
}

/** A run outside the timer; it does not move `nextAt`. */
export function runNow(id: string) {
  if (!findSchedule(id)) return;
  if (schedules.running?.id === id || schedules.queue.some((e) => e.id === id)) return;
  schedules.queue.push({ id, trigger: 'manual' });
  pump();
}

export function stopRun() {
  schedules.running?.stop?.();
}

export async function newSchedule(preset?: Partial<ScheduleInput>) {
  const connId = active().id ?? db.connections[0]?.id ?? '';
  const conn = db.connections.find((c) => c.id === connId);
  const initial: ScheduleInput = {
    name: '',
    enabled: true,
    notify: 'always',
    cadence: DEFAULT_CADENCE,
    task: { kind: 'query', connId, database: conn ? defaultDb(conn) : '', sql: '', allowWrites: false, prodAck: false },
    ...preset,
  };
  const answer = await ask<ScheduleInput | null>(ScheduleDialog, { initial, editing: false });
  if (!answer) return;
  openSchedule(createSchedule(answer).id);
}

export async function editSchedule(id: string) {
  const s = findSchedule(id);
  if (!s) return;
  const initial: ScheduleInput = $state.snapshot({ name: s.name, enabled: s.enabled, task: s.task, cadence: s.cadence, notify: s.notify });
  const answer = await ask<ScheduleInput | null>(ScheduleDialog, { initial, editing: true });
  if (!answer) return;
  updateSchedule(id, answer);
  openSchedule(id);
}

export function scheduleFromSaved(q: SavedQuery): Partial<ScheduleInput> {
  return { name: q.name, task: { kind: 'query', connId: q.connId, database: q.database, sql: q.sql, allowWrites: false, prodAck: false } };
}

let started = false;

/** Starts the one scheduler loop: the interval, plus a check when the window comes back to the front. */
export function startScheduler() {
  if (started) return;
  started = true;
  setInterval(tick, TICK_MS);
  document.addEventListener('visibilitychange', tick);
  window.addEventListener('focus', tick);
  tick();
}

function tick() {
  // Connections are not known yet; the next tick after the load picks the schedules up.
  if (db.loading || db.loadError) return;
  const now = Date.now();
  for (const s of schedules.list) {
    if (!s.enabled || s.nextAt > now) continue;
    if (schedules.running?.id === s.id || schedules.queue.some((e) => e.id === s.id)) continue;
    // `nextAt` moves forward on enqueue, so a missed period runs once, not once per missed slot.
    const trigger: Trigger = now - s.nextAt > 2 * TICK_MS ? 'catch-up' : 'schedule';
    schedules.queue.push({ id: s.id, trigger });
    s.nextAt = nextRun(s.cadence, now);
  }
  pump();
}

/** Takes the first runnable queue entry and starts it; only one run happens at a time. */
function pump() {
  if (schedules.running) return;
  // Drop entries whose schedule is gone. A prompt waits behind the user's live reply (the next tick
  // retries); a query behind it still runs.
  schedules.queue = schedules.queue.filter((e) => findSchedule(e.id));
  const i = schedules.queue.findIndex((e) => findSchedule(e.id)!.task.kind !== 'prompt' || !chat.live);
  if (i < 0) return;
  const [next] = schedules.queue.splice(i, 1);
  const s = findSchedule(next.id)!;
  void execute(s, next.trigger)
    .catch((e) => console.warn('The scheduled run failed.', e))
    .finally(() => {
      schedules.running = null;
      pump();
    });
}

type RunOutcome = Omit<ScheduleRun, 'id' | 'trigger' | 'started' | 'ended'>;

async function execute(s: Schedule, trigger: Trigger) {
  const began = Date.now();
  schedules.running = { id: s.id, started: began, stop: null };
  const outcome = s.task.kind === 'query' ? await runQueryTask(s) : await runPromptTask(s);
  if (outcome === null) {
    // Not run (a reply went live in between): the entry goes back at the front and nothing is recorded.
    schedules.queue.unshift({ id: s.id, trigger });
    return;
  }
  const run: ScheduleRun = { ...outcome, id: crypto.randomUUID(), trigger, started: began, ended: Date.now() };
  s.runs.unshift(run);
  // Trim the history: the newest RESULT_RUNS keep their rows and reply text, older ones only the summary.
  if (s.runs.length > MAX_RUNS) s.runs.length = MAX_RUNS;
  for (let i = RESULT_RUNS; i < s.runs.length; i++) {
    const r = s.runs[i];
    delete r.columns;
    delete r.rows;
    delete r.truncated;
    delete r.reply;
  }
  if (ui.page === 'scheduler' && schedules.selected === s.id && document.hasFocus()) s.seenAt = Date.now();
  announce(s, run);
}

async function runQueryTask(s: Schedule): Promise<RunOutcome> {
  const task = s.task as QueryTask;
  const c = db.connections.find((x) => x.id === task.connId);
  if (!c) return { status: 'error', message: 'The connection no longer exists.' };
  if (task.allowWrites && c.env === 'production' && !task.prodAck) {
    return { status: 'blocked', message: 'Write protection is on. Edit the schedule and confirm scheduled writes on this connection.' };
  }
  const began = Date.now();
  let kind: 'READ' | 'WRITE' = 'READ';
  try {
    if (!task.allowWrites) {
      const r = await api.runScheduledRead({ connId: task.connId, database: task.database || null, sql: task.sql, maxRows: STORED_ROWS });
      logQuery({ conn: c, kind, source: 'Schedule', sql: task.sql, result: { status: 'ok', rows: r.rows.length }, ms: r.elapsedMs });
      return { status: 'ok', columns: r.columns.map((x) => x.name), rows: r.rows, truncated: r.truncated, message: `${r.rows.length}${r.truncated ? '+' : ''} row${r.rows.length === 1 ? '' : 's'}` };
    }
    const cls = await classify(c.engine, task.sql, task.database);
    kind = cls.write ? 'WRITE' : 'READ';
    const queryId = crypto.randomUUID();
    if (schedules.running?.id === s.id) schedules.running.stop = () => void api.cancelQuery({ queryId }).catch(() => {});
    const run = await api.runQuery({ request: { connId: task.connId, database: task.database, sql: task.sql, queryId, limit: STORED_ROWS } });
    // The last statement that returned rows is the grid output.
    const grid = [...run.statements].reverse().find((x) => x.columns.length > 0);
    const last = run.statements[run.statements.length - 1];
    const message = grid
      ? `${grid.rows.length}${grid.truncated ? '+' : ''} row${grid.rows.length === 1 ? '' : 's'}`
      : run.statements.length === 1
        ? `${last?.affected ?? 0} rows affected`
        : `${run.statements.length} statements ran`;
    logQuery({ conn: c, kind, source: 'Schedule', sql: task.sql, result: { status: 'ok', rows: grid?.rowCount ?? last?.affected ?? 0 }, ms: Date.now() - began });
    if (cls.write) afterRun(c, cls, task.database);
    return { status: 'ok', columns: grid?.columns.map((x) => x.name), rows: grid?.rows.slice(0, STORED_ROWS), truncated: grid?.truncated, message };
  } catch (e) {
    const err = ApiError.from(e);
    const result = BLOCKED[err.code] ? ({ status: 'blocked' } as const) : ({ status: 'error', message: err.code === 'cancelled' ? 'Stopped.' : err.message } as const);
    logQuery({ conn: c, kind, source: 'Schedule', sql: task.sql, result, ms: Date.now() - began });
    return result.status === 'blocked' ? { status: 'blocked', message: err.message } : result;
  }
}

async function runPromptTask(s: Schedule): Promise<RunOutcome | null> {
  const task = s.task as PromptTask;
  const sources = task.sources.filter((x) => connOf(x));
  if (!sources.length) return { status: 'error', message: 'None of the databases of this schedule exist.' };
  if (schedules.running?.id === s.id) schedules.running.stop = () => chat.cancel?.();
  try {
    const res = await runScheduledPrompt({ scheduleId: s.id, title: `${s.name} · ${formatWhen(Date.now())}`, prompt: task.prompt, sources, skills: task.skills });
    // null: another reply went live while this started; the caller puts the entry back and records nothing.
    if (!res) return null;
    const first = (res.reply.split('\n').find((l) => l.trim()) ?? '').trim().slice(0, 140);
    return { status: res.ok ? 'ok' : 'error', chatId: res.sessionId, reply: res.reply.slice(0, REPLY_CHARS), message: res.ok ? first || 'Replied.' : res.error };
  } catch (e) {
    return { status: 'error', message: ApiError.from(e).message };
  }
}

function announce(s: Schedule, run: ScheduleRun) {
  if (s.notify === 'never') return;
  if (s.notify === 'failure' && run.status === 'ok') return;
  const text = `${s.name}: ${run.message}`;
  if (document.hasFocus()) notify(run.status === 'ok', text, { label: 'View', run: () => openSchedule(s.id) });
  else void osNotify(`sandb · ${s.name}`, run.status === 'ok' ? run.message : `Failed: ${run.message}`);
}
