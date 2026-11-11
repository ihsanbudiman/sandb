import { untrack } from 'svelte';
import { schemas, tableData, nsOf, type Row, type Cell } from './mock';
import { active, ui, db, canWrite, flushAi, type Conn } from './state.svelte';
import { activeTab, defaultDb } from './tabs.svelte';
import { api, ApiError, type CellValue, type ChartSpec, type ChatEvent, type ChatRequest, type ErrorCode, type WriteInfo } from './api';
import { logQuery, type AuditResult } from './audit.svelte';

/** `denied`: the user denied a proposed write, or the guard refused it. `failed`: the tool or the database returned an error. */
export type ToolStatus = 'running' | 'done' | 'blocked' | 'awaiting' | 'approved' | 'denied' | 'failed';
/**
 * `id`: the backend's tool call id (live replies only). `args`: the arguments the model sent.
 * `ms`: time from the call to its result. `truncated`: more rows exist than `rows` holds. `error`: the error code of a failed step.
 * `source`: the database the step ran on (live replies only).
 */
export type ToolPart = {
  kind: 'tool';
  id?: string;
  name: string;
  args?: Record<string, unknown>;
  sql?: string;
  status: ToolStatus;
  rows?: Row[];
  truncated?: boolean;
  note?: string;
  started?: number;
  ms?: number;
  error?: string;
  /** `write_preview` info: the guard's per-statement kinds and warnings, kept for the preview badge. */
  info?: WriteInfo;
  source?: Source;
};
/** `answer`: missing while the question is open, null when the user skipped it. */
export type QuestionPart = { kind: 'question'; text: string; options: string[]; answer?: string | null };
/** The reply stopped: an error, or the user pressed Stop. */
export type ErrorPart = { kind: 'error'; text: string };
/** A chart of a `render_chart` read. `rows` are arrays in `columns` order. `truncated`: the read had more rows than the chart keeps. */
export type ChartPart = { kind: 'chart'; spec: ChartSpec; columns: string[]; rows: CellValue[][]; truncated: boolean };
export type Part = { kind: 'text'; text: string } | ToolPart | QuestionPart | ErrorPart | ChartPart;
/** `started`/`ended`: when the assistant reply was sent and when it finished, stopped or failed (missing in chats saved before they were kept). */
export interface Msg {
  role: 'user' | 'assistant';
  parts: Part[];
  started?: number;
  ended?: number;
}
/** A database of a chat. `database` null: the connection's own database (chats saved before a chat had several databases). */
export interface Source {
  connId: string;
  database: string | null;
}
/** One conversation. It can use several databases, on one or more connections. */
export interface Session {
  id: string;
  sources: Source[];
  title: string;
  updated: number;
  messages: Msg[];
}

/** Databases in one chat; the backend refuses more. */
export const MAX_SOURCES = 8;

const KEY = 'sandb.chat.v1';
const MAX_SESSIONS = 50;

// A saved session can stop in the middle of a reply (the app closed). Its open steps can never finish.
function load(): Session[] {
  try {
    const list = JSON.parse(localStorage.getItem(KEY) ?? '[]') as (Session & { connId?: string })[];
    for (const s of list) {
      // Before a chat had several databases, it belonged to one connection.
      if (!s.sources) s.sources = s.connId ? [{ connId: s.connId, database: null }] : [];
      delete s.connId;
      for (const m of s.messages)
        for (const p of m.parts) {
          if (p.kind === 'tool' && (p.status === 'running' || p.status === 'awaiting')) {
            p.status = 'failed';
            p.note = 'Stopped: the app closed before this step finished. Nothing ran.';
          }
          if (p.kind === 'question' && p.answer === undefined) p.answer = null;
        }
    }
    return list;
  } catch (e) {
    console.warn('Chat history is not readable; starting empty.', e);
    return [];
  }
}

export const chat = $state({
  sessions: load(),
  /** The open session. null: the newest one; '': a new, empty chat. */
  open: null as string | null,
  /** Databases of the new, empty chat. null: the active tab's database, and it follows the tab. */
  draft: null as Source[] | null,
  /** Id of the session that the running reply writes to; null when the assistant is idle. */
  live: null as string | null,
  /** A proposed write that waits for Approve or Deny. */
  pending: null as null | { resolve: (ok: boolean) => void },
  /** A question to the user that waits for an answer. */
  question: null as null | { resolve: (answer: string | null) => void },
  /** Stops the running live reply; null when there is nothing to stop. */
  cancel: null as null | (() => void),
  /** A tool the model has started to call; its arguments still stream. null when there is none. */
  starting: null as string | null,
});

let saveTimer: ReturnType<typeof setTimeout> | undefined;
$effect.root(() => {
  $effect(() => {
    const json = JSON.stringify(chat.sessions);
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      try {
        localStorage.setItem(KEY, json);
      } catch (e) {
        console.warn('Chat history was not saved.', e);
      }
    }, 300);
  });
});

export const sessionsByDate = () => [...chat.sessions].sort((a, b) => b.updated - a.updated);

/** The session the panel shows. While a reply runs, that session, so its questions stay visible. */
export function current(): Session | undefined {
  if (chat.live) return chat.sessions.find((s) => s.id === chat.live);
  if (chat.open === '') return undefined;
  return (chat.open && chat.sessions.find((s) => s.id === chat.open)) || sessionsByDate()[0];
}

export function openSession(id: string) {
  if (chat.live) return;
  chat.open = id;
  chat.draft = null;
}

export const connOf = (s: Source): Conn | undefined => db.connections.find((c) => c.id === s.connId);
/** The database name of `s`; the connection's own database when the source has none. */
export const dbOf = (s: Source, c = connOf(s)) => s.database ?? (c ? defaultDb(c) : '');
export const sameSource = (a: Source, b: Source) => a.connId === b.connId && dbOf(a) === dbOf(b);

/** The active tab's connection and database; null while there is no connection, or when a MongoDB
 * tab has no database to query (the chat needs one). */
function tabSource(): Source | null {
  const c = active();
  if (!c.id) return null;
  const t = activeTab();
  const database = t?.connId === c.id ? t.database : defaultDb(c);
  if (c.engine === 'mongo' && !database) return null;
  return { connId: c.id, database };
}

/** Databases that `s` uses, or the new chat's databases. */
export function scopeOf(s: Session | undefined): Source[] {
  if (s) return s.sources;
  if (chat.draft) return chat.draft;
  const t = tabSource();
  return t ? [t] : [];
}

/** Sets the databases of the open chat. A change applies from the next question. */
export function setScope(list: Source[]) {
  if (chat.live) return;
  const s = current();
  if (s) s.sources = list;
  else chat.draft = list;
}

/** Opens the assistant on the active tab's database. A chat that does not use it gives way to a new chat; an empty chat takes the tab's database. */
export function openChatOnTab() {
  ui.chatOpen = true;
  const t = tabSource();
  if (!t || chat.live) return;
  const s = current();
  if (!s || !s.sources.some((x) => sameSource(x, t))) openSession('');
}

// The tab moved to another database: a chat that does not use it gives way to a new chat on it.
// Only a change of the tab's database acts, so a chat the user opens from history stays open.
let lastTab: string | undefined;
$effect.root(() => {
  $effect(() => {
    const t = tabSource();
    if (!t) return;
    const key = `${t.connId}\u0000${dbOf(t)}`;
    if (key === lastTab) return;
    lastTab = key;
    untrack(() => {
      if (chat.live) return;
      const s = current();
      if (s && !s.sources.some((x) => sameSource(x, t))) {
        chat.open = '';
        chat.draft = null;
      }
    });
  });
});

export function deleteSession(id: string) {
  if (id !== chat.live) chat.sessions = chat.sessions.filter((s) => s.id !== id);
}

export const decide = (ok: boolean) => chat.pending?.resolve(ok);
export const answer = (a: string | null) => chat.question?.resolve(a);

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function stream(msg: Msg, text: string) {
  msg.parts.push({ kind: 'text', text: '' });
  const part = msg.parts[msg.parts.length - 1] as { kind: 'text'; text: string };
  for (const w of text.split(/(\s+)/)) {
    part.text += w;
    await sleep(18);
  }
}

async function tool(msg: Msg, t: ToolPart, ms = 600) {
  msg.parts.push({ ...t, args: t.args ?? (t.name === 'describe_table' ? { table: t.sql } : { sql: t.sql }), started: Date.now() });
  const live = msg.parts[msg.parts.length - 1] as ToolPart;
  await sleep(ms);
  return live;
}

function finish(t: ToolPart) {
  t.status = 'done';
  if (t.started) t.ms = Date.now() - t.started;
}

/** Rows of a read that the chat keeps; the backend sends the same number (`UI_ROWS`). */
const CHAT_ROWS = 50;

/** Runs the text of a finished read step again on the AI read path and replaces its rows. */
export async function rerun(t: ToolPart) {
  if ((t.name !== 'run_read_query' && t.name !== 'run_mongo_read') || !t.sql || t.status === 'running') return;
  // Steps saved before they kept their database run on the active tab, as they did then.
  const conn = t.source ? connOf(t.source) : active();
  const tab = activeTab();
  const database = t.source ? t.source.database : tab?.connId === conn?.id && tab?.database ? tab.database : null;
  if (!conn) {
    t.status = 'failed';
    t.note = 'The connection of this step was deleted.';
    return;
  }
  t.status = 'running';
  t.started = Date.now();
  t.error = undefined;
  try {
    const r = await api.runAiRead(conn.id, database, t.sql);
    const cols = r.columns.map((c) => c.name);
    t.rows = r.rows.slice(0, CHAT_ROWS).map((x) => Object.fromEntries(cols.map((c, i) => [c, x[i] as Cell])));
    t.truncated = r.truncated || r.rows.length > CHAT_ROWS;
    t.note = `${r.rows.length}${r.truncated ? ' (row limit reached)' : ''} row${r.rows.length === 1 ? '' : 's'} · ${r.elapsedMs} ms`;
    finish(t);
    logQuery({ conn, kind: 'READ', source: 'AI', sql: t.sql, result: { status: 'ok', rows: r.rows.length }, ms: t.ms ?? 0 });
  } catch (e) {
    const err = ApiError.from(e);
    t.status = BLOCKED[err.code] ? 'blocked' : 'failed';
    t.note = err.message;
    t.error = err.code;
    t.ms = Date.now() - t.started;
    logQuery({ conn, kind: 'READ', source: 'AI', sql: t.sql, result: BLOCKED[err.code] ? { status: 'blocked' } : { status: 'error', message: err.message }, ms: 0 });
  }
}

async function ask(msg: Msg, text: string, options: string[]) {
  msg.parts.push({ kind: 'question', text, options });
  const part = msg.parts[msg.parts.length - 1] as QuestionPart;
  ui.chatOpen = true;
  const a = await new Promise<string | null>((resolve) => {
    chat.question = { resolve };
  });
  chat.question = null;
  part.answer = a;
  return a;
}

export async function send(text: string) {
  if (!text.trim() || chat.live) return;
  let s = current();
  const sources = scopeOf(s).filter((x) => connOf(x));
  if (!sources.length) return;
  if (!s) {
    chat.sessions.unshift({ id: crypto.randomUUID(), sources: sources.map((x) => ({ ...x })), title: text.trim(), updated: Date.now(), messages: [] });
    s = chat.sessions[0];
    chat.draft = null;
    if (chat.sessions.length > MAX_SESSIONS) {
      const oldest = [...chat.sessions].sort((a, b) => a.updated - b.updated)[0];
      chat.sessions = chat.sessions.filter((x) => x !== oldest);
    }
  }
  chat.open = s.id;
  chat.live = s.id;
  s.updated = Date.now();
  const history = historyOf(s.messages);
  s.messages.push({ role: 'user', parts: [{ kind: 'text', text }] });
  s.messages.push({ role: 'assistant', parts: [], started: Date.now() });
  const msg = s.messages[s.messages.length - 1];
  try {
    // Mock mode scripts SQL flows only; a Mongo source anywhere gives a plain answer instead of fake steps.
    const mongoConn = sources.map((x) => connOf(x)).find((c) => c?.engine === 'mongo');
    if (api.kind === 'mock') await reply(msg, text, mongoConn ?? connOf(sources[0])!, !!mongoConn);
    else await liveReply(msg, text, sources, history);
  } finally {
    msg.ended = Date.now();
    chat.live = chat.pending = chat.question = chat.cancel = chat.starting = null;
  }
}

// Earlier turns as plain text: the user's message, or the assistant's text parts. Tool steps stay out.
function historyOf(messages: Msg[]): ChatRequest['history'] {
  return messages.flatMap((m) => {
    const content = m.parts
      .flatMap((p) => (p.kind === 'text' ? [p.text] : []))
      .join('\n\n')
      .trim();
    return content ? [{ role: m.role, content }] : [];
  });
}

/** Codes of a step the guard or the write policy refused: `BLOCKED`, never `FAILED`. */
const BLOCKED: Partial<Record<ErrorCode, true>> = { guard_rejected: true, write_blocked: true, read_only_violation: true, connection_read_only: true };

/** First number of a summary such as "5 rows · 42 ms". */
const countIn = (summary: string) => Number(/^([\d,]+)/.exec(summary)?.[1].replace(/,/g, '') ?? NaN);

// Real backends: the Rust tool loop streams its steps; this maps each event onto the reply's parts.
async function liveReply(msg: Msg, text: string, sources: Source[], history: ChatRequest['history']) {
  const chatId = crypto.randomUUID();
  let stopped = false;
  let ended = false;
  chat.cancel = () => {
    stopped = true;
    api.chatCancel(chatId).catch((e) => console.warn('The reply was not stopped.', ApiError.from(e).message));
  };
  const fail = (code: string, message: string) => {
    msg.parts.push({ kind: 'error', text: code === 'cancelled' ? 'Stopped.' : message });
  };

  try {
    await flushAi();
  } catch (e) {
    const err = ApiError.from(e);
    fail(err.code, `The AI settings are not saved: ${err.message}`);
    return;
  }
  if (stopped) return fail('cancelled', '');

  // `send` keeps only sources whose connection exists.
  const conns = sources.map((x) => connOf(x)!);
  const request: ChatRequest = {
    chatId,
    sources: sources.map((x, i) => ({ connId: x.connId, database: x.database, schema: null, allowWrites: conns[i].aiWrite && canWrite(conns[i]) })),
    history,
    message: text,
  };
  /** The source a tool call names (`db1` is the first); the only one when it names none. */
  const sourceIn = (args: Record<string, unknown>) => {
    const n = typeof args.source === 'string' ? /^db(\d+)$/.exec(args.source)?.[1] : undefined;
    const i = n ? Number(n) - 1 : sources.length === 1 ? 0 : -1;
    return i >= 0 && i < sources.length ? i : -1;
  };
  const steps = new Map<string, { part: ToolPart; started: number; conn: Conn }>();

  const onEvent = (e: ChatEvent) => {
    switch (e.type) {
      case 'text': {
        const last = msg.parts.at(-1);
        if (last?.kind === 'text') last.text += e.delta;
        else msg.parts.push({ kind: 'text', text: e.delta });
        break;
      }
      case 'tool_starting':
        chat.starting = e.name;
        break;
      case 'tool_call': {
        if (chat.starting === e.name) chat.starting = null;
        const sql = typeof e.args.sql === 'string' ? e.args.sql : typeof e.args.query === 'string' ? e.args.query : typeof e.args.table === 'string' ? e.args.table : undefined;
        const started = Date.now();
        const i = sourceIn(e.args);
        const source = i < 0 ? undefined : { ...sources[i] };
        msg.parts.push({ kind: 'tool', id: e.id, name: e.name, args: e.args, sql, status: 'running', started, source });
        // A call that names no valid source fails in the backend; its audit entry goes to the first database.
        steps.set(e.id, { part: msg.parts[msg.parts.length - 1] as ToolPart, started, conn: conns[Math.max(i, 0)] });
        break;
      }
      case 'write_preview': {
        const step = steps.get(e.id);
        if (!step) break;
        const { part } = step;
        part.sql = e.sql;
        part.info = e.info;
        part.status = 'awaiting';
        ui.chatOpen = true;
        // The backend waits for this decision; its `tool_result` follows.
        chat.pending = {
          resolve: (ok) => {
            chat.pending = null;
            part.status = 'running';
            if (ok) part.note = 'Running the write…';
            (ok ? api.approveWrite : api.denyWrite)(chatId, e.id).catch((err) => {
              part.note = ApiError.from(err).message;
            });
          },
        };
        break;
      }
      case 'chart':
        msg.parts.push({ kind: 'chart', spec: e.spec, columns: e.columns, rows: e.rows, truncated: e.truncated });
        break;
      case 'tool_result': {
        const step = steps.get(e.id);
        if (!step) break;
        const { part, started, conn } = step;
        const blocked = !e.ok && !!e.error && !!BLOCKED[e.error.code];
        const denied = !e.ok && e.error?.code === 'write_denied';
        const write = isWriteTool(part.name);
        part.status = e.ok ? (write ? 'approved' : 'done') : denied ? 'denied' : blocked ? 'blocked' : 'failed';
        part.note = e.ok ? e.summary : (e.error?.message ?? e.summary);
        part.ms = Date.now() - started;
        if (e.error) part.error = e.error.code;
        const columns = e.columns;
        if (e.ok && columns && e.rows?.length) part.rows = e.rows.map((r) => Object.fromEntries(columns.map((c, i) => [c, r[i] as Cell])));
        if (e.truncated) part.truncated = true;
        if ((part.name === 'run_read_query' || part.name === 'run_mongo_read') && part.sql) {
          const n = countIn(e.summary);
          const result: AuditResult = e.ok ? { status: 'ok', rows: Number.isFinite(n) ? n : (e.rows?.length ?? 0) } : blocked ? { status: 'blocked' } : { status: 'error', message: e.error?.message ?? e.summary };
          logQuery({ conn, kind: 'READ', source: 'AI', sql: part.sql, result, ms: e.ok ? Date.now() - started : 0 });
        }
        if (write && part.sql) {
          const n = countIn(e.summary);
          const ms = Number(/(\d+) ms$/.exec(e.summary)?.[1] ?? 0);
          const result: AuditResult = e.ok ? { status: 'ok', rows: Number.isFinite(n) ? n : 0 } : e.error?.code === 'write_denied' ? { status: 'denied' } : blocked ? { status: 'blocked' } : { status: 'error', message: e.error?.message ?? e.summary };
          logQuery({ conn, kind: 'WRITE', source: 'AI', sql: part.sql, result, ms: e.ok ? ms : 0 });
        }
        break;
      }
      case 'error':
        ended = true;
        fail(e.error.code, e.error.message);
        break;
      case 'done':
        ended = true;
        break;
    }
  };

  try {
    await api.chatSend(request, onEvent);
  } catch (e) {
    if (!ended) {
      const err = ApiError.from(e);
      fail(err.code, err.message);
    }
  } finally {
    // Steps without a result can never finish now. A write still waiting for approval never ran:
    // the audit log gets the same WRITE/denied row a Deny click would leave.
    for (const { part, conn } of steps.values()) {
      if (part.status === 'running') {
        part.status = 'failed';
        part.note = 'Stopped before this step finished.';
      } else if (part.status === 'awaiting' && isWriteTool(part.name) && part.sql) {
        part.status = 'failed';
        part.note = 'Stopped: the write was not approved, nothing ran.';
        logQuery({ conn, kind: 'WRITE', source: 'AI', sql: part.sql, result: { status: 'denied' }, ms: 0 });
      }
    }
  }
}

const WRITE = /\b(delete|remove|update|insert|drop|truncate|alter|mark)\b/i;

/** The propose_* tools: their result is a write that needs (or got) approval. */
const isWriteTool = (name: string) => name === 'propose_write' || name === 'propose_mongo_write';
const find = (tables: string[], name: string) => tables.find((t) => t.toLowerCase() === name.toLowerCase());

// Prototype only: scripted agent that mimics the real tool-calling loop.
async function reply(msg: Msg, text: string, conn: Conn, mongo = false) {
  // Mock mode has no scripted Mongo flow: a Mongo source gets a plain answer, not fake tool steps.
  if (mongo || conn.engine === 'mongo') {
    await stream(msg, `I can read MongoDB collections on **${conn.name}**, but the demo build has no scripted Mongo answers. Ask a question about a collection and the live assistant will query it.`);
    return;
  }
  const schema = schemas[conn.schema];
  const tables = Object.keys(schema);
  const wantsSqlBlock = /\bsql\s+(?:block|code|snippet)\b/i.test(text);

  if (WRITE.test(text) && !wantsSqlBlock) {
    const t1 = await tool(msg, { kind: 'tool', name: 'describe_table', sql: tables[0], status: 'running' }, 500);
    finish(t1);
    t1.note = `${schema[tables[0]].length} columns · 48,210 rows`;
    const sql = "DELETE FROM public.orders\nWHERE status = 'cancelled'\n  AND created_at < now() - interval '90 days';";
    const c = await tool(msg, { kind: 'tool', name: 'run_read_query', sql: "SELECT count(*) FROM public.orders\nWHERE status = 'cancelled' AND created_at < now() - interval '90 days';", status: 'running' });
    finish(c);
    c.rows = [{ count: 214 }];
    logQuery({ conn, kind: 'READ', source: 'AI', sql: c.sql!, result: { status: 'ok', rows: 1 }, ms: 58 });
    const w = await tool(msg, { kind: 'tool', name: 'propose_write', sql, status: conn.aiWrite ? 'awaiting' : 'blocked' }, 300);

    if (!conn.aiWrite) {
      w.note = `Blocked: the assistant is read-only on ${conn.name}.`;
      logQuery({ conn, kind: 'WRITE', source: 'AI', sql, result: { status: 'blocked' }, ms: 0 });
      await stream(msg, "This would delete **214** cancelled orders older than 90 days. I can't run it because the assistant is read-only. Turn on write mode with the shield button, or copy the SQL into the editor and run it yourself.");
    } else {
      ui.chatOpen = true;
      const ok = await new Promise<boolean>((resolve) => {
        chat.pending = { resolve };
      });
      chat.pending = null;
      w.status = ok ? 'approved' : 'denied';
      w.note = ok ? '214 rows affected · committed' : 'You denied this statement. Nothing changed.';
      logQuery({ conn, kind: 'WRITE', source: 'AI', sql, result: ok ? { status: 'ok', rows: 214 } : { status: 'denied' }, ms: ok ? 296 : 0 });
      await stream(msg, ok ? 'Done. I deleted 214 cancelled orders inside one transaction. The change is in the audit log.' : 'OK, I did not run it. The data is unchanged.');
    }
    return;
  }

  // Mock only: an answer with a SQL code block, to exercise the Run menu in the chat.
  if (/\bjson\s+(?:block|code)\b/i.test(text)) {
    await stream(msg, 'Here is the payload:\n\n```json\n{"status":"paid","orders":3}\n```\n\nCopy it or use it as a reference.');
    return;
  }
  if (wantsSqlBlock) {
    const write = /\b(?:update|delete|insert|drop|write)\b/i.test(text);
    const sql = write
      ? "UPDATE public.orders\nSET status = 'paid'\nWHERE id = 1;"
      : 'SELECT *\nFROM public.orders\nLIMIT 10;';
    const tail = write ? 'Use Insert into editor to review it before running.' : 'Pick Run to execute it.';
    await stream(msg, `Here is the query:\n\n\`\`\`sql\n${sql}\n\`\`\`\n\n${tail}`);
    return;
  }

  // A table named in the message ("table notes", "from notes", "from table notes"), else any table name in the text.
  const named = /\b(?:from|table)\s+(?:table\s+)?[`"']?(\w+)/i.exec(text)?.[1];
  let target = named ? find(tables, named) : tables.find((t) => new RegExp(`\\b${t}\\b`, 'i').test(text));

  if (!named && schema.orders && /\b(chart|graph|plot|report)\b/i.test(text)) {
    const q = await tool(msg, { kind: 'tool', name: 'render_chart', sql: 'SELECT status, sum(total) AS revenue\nFROM public.orders\nGROUP BY status\nORDER BY revenue DESC;', status: 'running' }, 800);
    finish(q);
    q.note = 'Bar chart · 5 rows · 84 ms';
    msg.parts.push({
      kind: 'chart',
      spec: { type: 'bar', title: 'Revenue by order status', x: 'status', y: ['revenue'], stacked: false },
      columns: ['status', 'revenue'],
      rows: [['paid', 1843210.5], ['shipped', 1290442.1], ['pending', 402118.0], ['cancelled', 0], ['refunded', -98220.4]],
      truncated: false,
    });
    const m = await tool(msg, { kind: 'tool', name: 'render_chart', sql: "SELECT date_trunc('month', created_at) AS month, sum(total) AS revenue, count(*) AS orders\nFROM public.orders\nGROUP BY 1\nORDER BY 1;", status: 'running' }, 800);
    finish(m);
    m.note = 'Line chart · 6 rows · 61 ms';
    msg.parts.push({
      kind: 'chart',
      spec: { type: 'line', title: 'Monthly revenue', x: 'month', y: ['revenue'], stacked: false },
      columns: ['month', 'revenue'],
      rows: [['2026-05', 512300], ['2026-06', 548120], ['2026-07', 601880], ['2026-08', 587400], ['2026-09', 642950], ['2026-10', 695210]],
      truncated: false,
    });
    await stream(
      msg,
      '# Orders revenue report\n\nRevenue grew in five of the last six months. **Paid** orders bring most of the revenue.\n\n## Revenue by status\n\n| Status | Revenue |\n|---|---:|\n| paid | $1,843,210 |\n| shipped | $1,290,442 |\n| pending | $402,118 |\n| refunded | −$98,220 |\n\n## Findings\n\n- October is the best month so far ($695K).\n- Refunds remove about 2.5% of gross revenue.\n- About 8% of orders are cancelled.',
    );
    return;
  }

  if (!target && !named && schema.orders && /\b(orders?|revenue)\b/i.test(text)) {
    const t1 = await tool(msg, { kind: 'tool', name: 'describe_table', sql: 'orders', status: 'running' }, 500);
    finish(t1);
    t1.note = `${schema.orders.length} columns · 48,210 rows`;
    const q = await tool(msg, { kind: 'tool', name: 'run_read_query', sql: 'SELECT status, count(*) AS orders, sum(total) AS revenue\nFROM public.orders\nGROUP BY status\nORDER BY revenue DESC;', status: 'running' }, 800);
    finish(q);
    logQuery({ conn, kind: 'READ', source: 'AI', sql: q.sql!, result: { status: 'ok', rows: 5 }, ms: 84 });
    q.rows = [
      { status: 'paid', orders: 21480, revenue: 1843210.5 },
      { status: 'shipped', orders: 15120, revenue: 1290442.1 },
      { status: 'pending', orders: 6230, revenue: 402118.0 },
      { status: 'cancelled', orders: 3910, revenue: 0 },
      { status: 'refunded', orders: 1470, revenue: -98220.4 },
    ];
    await stream(msg, '**Paid** orders bring the most revenue ($1.84M from 21,480 orders), then **shipped** ($1.29M). About 8% of orders are cancelled, and refunds remove about $98K.');
    return;
  }

  if (!target) {
    await sleep(400);
    const question = named ? `I can't find a table named **${named}** on ${conn.name}. Which table do you mean?` : `Which table on ${conn.name} should I look at?`;
    const a = await ask(msg, question, tables);
    if (a === null) {
      await stream(msg, 'OK. Tell me which table to use when you are ready.');
      return;
    }
    target = find(tables, a.trim()) ?? tables.find((t) => new RegExp(`\\b${t}\\b`, 'i').test(a));
    if (!target) {
      await stream(msg, `I can't find **${a.trim()}** either. The tables on ${conn.name} are: ${tables.join(', ')}.`);
      return;
    }
  }

  const data = tableData(schema, target);
  const total = data.base.length + data.added.length;
  const t1 = await tool(msg, { kind: 'tool', name: 'describe_table', sql: target, status: 'running' }, 500);
  finish(t1);
  t1.note = `${schema[target].length} columns · ${total.toLocaleString()} rows`;
  const q = await tool(msg, { kind: 'tool', name: 'run_read_query', sql: `SELECT *\nFROM ${nsOf(conn.engine, target)}.${target}\nLIMIT 5;`, status: 'running' }, 700);
  finish(q);
  q.rows = data.base.slice(0, 5);
  logQuery({ conn, kind: 'READ', source: 'AI', sql: q.sql!, result: { status: 'ok', rows: q.rows.length }, ms: 41 });
  await stream(msg, `Here are the first ${q.rows.length} rows of **${target}** (${total.toLocaleString()} rows in total). Ask me to filter or summarize them.`);
}
