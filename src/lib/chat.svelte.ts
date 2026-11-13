import { untrack } from 'svelte';
import { api, ApiError, type Cell, type ChatEvent, type ChatRequest, type ErrorCode, type ToolKind } from './api';
import { logQuery, type AuditResult } from './audit.svelte';
import { afterWrite } from './catalog.svelte';
import { type Msg, type Session, type ToolPart } from './chatModel';
import { UI_ROWS } from './api/generated/constants';
import { isReadTool, isWriteTool } from './chatTools';
import { canWrite, connOf, dbOf, defaultDb, sameSource, type Conn, type Source } from './connections.svelte';
import { autosave, readStored } from './persist.svelte';
import { ui, flushAi } from './state.svelte';
import { active, activeTab } from './tabs.svelte';

const KEY = 'sandb.chat.v1';
const MAX_SESSIONS = 50;

/** Kinds of steps saved before the backend sent them on the wire; a name from a newer build is `other`. */
const LEGACY_KIND: Record<string, ToolKind> = { list_tables: 'list', describe_table: 'describe', run_read_query: 'read', run_mongo_read: 'read', render_chart: 'chart', render_mongo_chart: 'chart', propose_write: 'write', propose_mongo_write: 'write' };

// A saved session can stop in the middle of a reply (the app closed). Its open steps can never finish.
function normalize(list: (Session & { connId?: string })[]): Session[] {
  for (const s of list) {
    // Before a chat had several databases, it belonged to one connection.
    if (!s.sources) s.sources = s.connId ? [{ connId: s.connId, database: null }] : [];
    delete s.connId;
    for (const m of s.messages) {
      // Chats saved before the demo asked questions in the reply keep only the parts the UI knows.
      m.parts = m.parts.filter((p) => (p.kind as string) !== 'question');
      for (const p of m.parts) {
        if (p.kind === 'tool' && !p.tool) p.tool = LEGACY_KIND[p.name] ?? 'other';
        if (p.kind === 'tool' && (p.status === 'running' || p.status === 'awaiting')) {
          p.status = 'failed';
          p.note = 'Stopped: the app closed before this step finished. Nothing ran.';
        }
      }
    }
  }
  return list;
}

export const chat = $state({
  sessions: readStored<Session[]>(KEY, [], {
    migrate: (data) => (Array.isArray(data) ? normalize(data as (Session & { connId?: string })[]) : []),
  }),
  /** The open session. null: the newest one; '': a new, empty chat. */
  open: null as string | null,
  /** Databases of the new, empty chat. null: the active tab's database, and it follows the tab. */
  draft: null as Source[] | null,
  /** Id of the session that the running reply writes to; null when the assistant is idle. */
  live: null as string | null,
  /** A proposed write that waits for Approve or Deny. */
  pending: null as null | { resolve: (ok: boolean) => void },
  /** Stops the running live reply; null when there is nothing to stop. */
  cancel: null as null | (() => void),
  /** A tool the model has started to call; its arguments still stream. null when there is none. */
  starting: null as null | { kind: ToolKind; name: string },
});

autosave(KEY, () => chat.sessions, { delay: 300, onError: (e) => console.warn('Chat history was not saved.', e) });

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

function finish(t: ToolPart) {
  t.status = 'done';
  if (t.started) t.ms = Date.now() - t.started;
}

/** Runs the text of a finished read step again on the AI read path and replaces its rows. */
export async function rerun(t: ToolPart) {
  if (!isReadTool(t) || !t.sql || t.status === 'running') return;
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
    const r = await api.runAiRead({ connId: conn.id, database, sql: t.sql });
    const cols = r.columns.map((c) => c.name);
    t.rows = r.rows.slice(0, UI_ROWS).map((x) => Object.fromEntries(cols.map((c, i) => [c, x[i] as Cell])));
    t.truncated = r.truncated || r.rows.length > UI_ROWS;
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
    await liveReply(msg, text, sources, history);
  } finally {
    msg.ended = Date.now();
    chat.live = chat.pending = chat.cancel = chat.starting = null;
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

// Real backends: the Rust tool loop streams its steps; this maps each event onto the reply's parts.
async function liveReply(msg: Msg, text: string, sources: Source[], history: ChatRequest['history']) {
  const chatId = crypto.randomUUID();
  let stopped = false;
  let ended = false;
  chat.cancel = () => {
    stopped = true;
    api.chatCancel({ chatId }).catch((e) => console.warn('The reply was not stopped.', ApiError.from(e).message));
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
        chat.starting = { kind: e.kind, name: e.name };
        break;
      case 'tool_call': {
        if (chat.starting?.name === e.name) chat.starting = null;
        const sql = typeof e.args.sql === 'string' ? e.args.sql : typeof e.args.query === 'string' ? e.args.query : typeof e.args.table === 'string' ? e.args.table : undefined;
        const started = Date.now();
        const i = sourceIn(e.args);
        const source = i < 0 ? undefined : { ...sources[i] };
        msg.parts.push({ kind: 'tool', tool: e.kind, id: e.id, name: e.name, args: e.args, sql, status: 'running', started, source });
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
            (ok ? api.approveWrite({ chatId, id: e.id }) : api.denyWrite({ chatId, id: e.id })).catch((err) => {
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
        const write = isWriteTool(part);
        part.status = e.ok ? (write ? 'approved' : 'done') : denied ? 'denied' : blocked ? 'blocked' : 'failed';
        part.note = e.ok ? e.summary : (e.error?.message ?? e.summary);
        part.ms = Date.now() - started;
        if (e.error) part.error = e.error.code;
        const columns = e.columns;
        if (e.ok && columns && e.rows?.length) part.rows = e.rows.map((r) => Object.fromEntries(columns.map((c, i) => [c, r[i] as Cell])));
        if (e.truncated) part.truncated = true;
        if (isReadTool(part) && part.sql) {
          const rows = e.result?.rows ?? e.rows?.length ?? 0;
          const result: AuditResult = e.ok ? { status: 'ok', rows } : blocked ? { status: 'blocked' } : { status: 'error', message: e.error?.message ?? e.summary };
          logQuery({ conn, kind: 'READ', source: 'AI', sql: part.sql, result, ms: e.ok ? Date.now() - started : 0 });
        }
        if (write && part.sql) {
          const rows = e.result?.rows ?? 0;
          const ms = e.result?.ms ?? 0;
          const result: AuditResult = e.ok ? { status: 'ok', rows } : e.error?.code === 'write_denied' ? { status: 'denied' } : blocked ? { status: 'blocked' } : { status: 'error', message: e.error?.message ?? e.summary };
          logQuery({ conn, kind: 'WRITE', source: 'AI', sql: part.sql, result, ms: e.ok ? ms : 0 });
          // An approved write can change the schema or create a collection the tree does not list.
          if (e.ok) void afterWrite(conn, part.sql, part.source, part.info);
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
      } else if (part.status === 'awaiting' && isWriteTool(part) && part.sql) {
        part.status = 'failed';
        part.note = 'Stopped: the write was not approved, nothing ran.';
        logQuery({ conn, kind: 'WRITE', source: 'AI', sql: part.sql, result: { status: 'denied' }, ms: 0 });
      }
    }
  }
}
