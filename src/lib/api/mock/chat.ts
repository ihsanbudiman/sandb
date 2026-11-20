import { ApiError } from '../types';
import { CHART_ROWS, REDIS_COMMANDS, UI_ROWS } from '../generated/constants';
import type { AppError, CellValue, ChatEvent, ChatRequest, ToolKind, ToolResultData, WriteInfo } from '../generated/wire';
import { mockSchema, nsOf, tableData } from './data';
import { skillTitle } from '../../skills';
import type { Row } from '../types';
import { ai, find } from './state';
import { redisAiRead } from './redisRun';

interface Script {
  cancelled: boolean;
  pending: { id: string; resolve: (ok: boolean) => void } | null;
}

const scripts = new Map<string, Script>();

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Thrown inside a script after Stop, so no further events are sent. */
class Cancelled extends Error {}

/** The demo's scripted assistant: the same events the real backend streams, produced from canned steps. */
export async function chatStream(request: ChatRequest, emit: (e: ChatEvent) => void): Promise<void> {
  const script: Script = { cancelled: false, pending: null };
  scripts.set(request.chatId, script);
  try {
    await reply(request, script, emit);
    if (script.cancelled) throw new Cancelled();
    emit({ type: 'done' });
  } catch (e) {
    if (e instanceof Cancelled) emit({ type: 'error', error: { code: 'cancelled', message: 'Stopped.' } });
    else {
      const err = ApiError.from(e);
      emit({ type: 'error', error: { code: err.code, message: err.message, ...(err.detail ? { detail: err.detail } : {}) } });
    }
  } finally {
    scripts.delete(request.chatId);
  }
}

/** Stops a reply; its script ends with a `cancelled` error event. */
export function chatCancel(chatId: string) {
  const script = scripts.get(chatId);
  if (!script) return;
  script.cancelled = true;
  script.pending?.resolve(false);
  script.pending = null;
}

export function decideWrite(chatId: string, id: string, ok: boolean) {
  const script = scripts.get(chatId);
  if (!script?.pending || script.pending.id !== id) throw new ApiError({ code: 'not_found', message: 'This write no longer waits for a decision' });
  const { resolve } = script.pending;
  script.pending = null;
  resolve(ok);
}

const WRITE = /\b(delete|remove|update|insert|drop|truncate|alter|mark)\b/i;
const REDIS_WRITE = /\bwrite\b|^\s*(?:please\s+)?(?:add|set|change|rename|expire|flush|persist)\b/i;
const REDIS_NAMES = new Set<string>(REDIS_COMMANDS);
const REDIS_CHART = /\b(?:chart|graph|plot)\b/i;

async function reply(request: ChatRequest, script: Script, emit: (e: ChatEvent) => void) {
  const text = request.message;
  const conns = request.sources.map((s) => find(s.connId));
  const mongoConn = conns.find((c) => c.engine === 'mongo');
  const redisConn = /\bredis\b/i.test(text) ? conns.find((c) => c.engine === 'redis') : undefined;
  const conn = redisConn ?? mongoConn ?? conns[0];
  if (!conn) throw new ApiError({ code: 'not_found', message: 'No connection for this chat' });
  const sourceIndex = request.sources.findIndex((s) => s.connId === conn.id);
  const source = { source: `db${sourceIndex + 1}` };
  const allowWrites = !!request.sources[sourceIndex]?.allowWrites;
  const waiting = async (ms: number) => {
    if (script.cancelled) throw new Cancelled();
    await sleep(ms);
    if (script.cancelled) throw new Cancelled();
  };
  const stream = async (body: string, type: 'text' | 'reasoning' = 'text') => {
    for (const word of body.split(/(\s+)/)) {
      if (script.cancelled) throw new Cancelled();
      emit({ type, delta: word });
      await sleep(18);
    }
  };
  const call = (kind: ToolKind, name: string, args: Record<string, CellValue>) => {
    const id = crypto.randomUUID();
    emit({ type: 'tool_call', id, name, kind, args: { ...args, ...source } });
    return id;
  };
  const done = (id: string, summary: string, extra: { columns?: string[]; rows?: CellValue[][]; result?: ToolResultData; error?: AppError; truncated?: boolean } = {}) =>
    emit({ type: 'tool_result', id, ok: !extra.error, summary, ...extra });
  const rowsOf = (rows: Row[], columns: string[]) => rows.map((r) => columns.map((c) => r[c]));

  // Picked skills open the reply; asking about a skill without a pick makes the assistant load one by itself.
  const picked = request.skills ?? [];
  if (picked.length) {
    await stream(`Using skills: ${picked.map(skillTitle).join(', ')}.\n\n`);
  } else if (/\bskill\b/i.test(text)) {
    await waiting(300);
    const skill = conn.engine === 'redis' ? 'redis' : 'data-quality';
    const s = call('skill', 'load_skill', { name: skill });
    await waiting(500);
    done(s, skillTitle(skill));
  }

  if (conn.engine === 'redis') {
    const raw = text.trim();
    const name = raw.split(/\s+/, 1)[0].toUpperCase();
    const direct = REDIS_NAMES.has(name);
    if (!direct && (WRITE.test(text) || REDIS_WRITE.test(text))) {
      await stream('The demo cannot run Redis writes.');
      return;
    }

    const chart = !direct && REDIS_CHART.test(text);
    const spec = { type: 'bar', title: 'Leaderboard scores', x: 'member', y: ['score'], stacked: false };
    const command = direct ? raw : chart || /\bleaderboard\b/i.test(text) ? 'ZRANGE leaderboard 0 9 REV WITHSCORES'
      : /\buser:1\b|\bredis read\b/i.test(text) ? 'HGETALL user:1' : /\bgreeting\b/i.test(text) ? 'GET greeting'
        : /\bhow many\b|\bcount\b/i.test(text) ? 'DBSIZE' : 'SCAN 0 COUNT 100';
    const id = chart ? call('chart', 'render_redis_chart', { command, ...spec }) : call('read', 'run_redis_read', { command });
    await waiting(500);
    try {
      const r = redisAiRead(conn, request.sources[sourceIndex].database ?? null, command, ai.settings.maxRows, 'assistant');
      const columns = r.columns.map((c) => c.name);
      const count = `${r.rows.length} ${r.rows.length === 1 ? 'row' : 'rows'}`;
      const summary = `${chart ? 'Bar chart · ' : ''}${count} · ${r.elapsedMs} ms${r.notice ? ` · ${r.notice}` : ''}`;
      if (chart) emit({ type: 'chart', id, spec, columns, rows: r.rows.slice(0, CHART_ROWS), truncated: r.truncated || r.rows.length > CHART_ROWS });
      done(id, summary, { result: { rows: r.rows.length, ms: r.elapsedMs }, ...(chart ? {} : { columns, rows: r.rows.slice(0, UI_ROWS), truncated: r.truncated || r.rows.length > UI_ROWS }) });
      const answer = command.toUpperCase() === 'DBSIZE' ? `This Redis database has **${r.rows[0]?.[0]} keys**.` : `Redis returned **${count}**.`;
      await stream(`${answer}${r.notice ? ` ${r.notice}.` : ''}`);
    } catch (e) {
      if (e instanceof Cancelled) throw e;
      const { code, message, detail } = ApiError.from(e);
      const note = code === 'guard_rejected' && message.endsWith(' is not a read') ? `The demo cannot run Redis writes. ${message}` : message;
      done(id, note, { error: { code, message: note, ...(detail ? { detail } : {}) } });
      await stream(note);
    }
    return;
  }

  if (conn.engine === 'mongo') {
    await stream(`I can read MongoDB collections on **${conn.name}**, but the demo build has no scripted Mongo answers. Ask a question about a collection and the live assistant will query it.`);
    return;
  }

  const schema = mockSchema(conn);
  const tables = Object.keys(schema);
  const wantsSqlBlock = /\bsql\s+(?:block|code|snippet)\b/i.test(text);

  if (WRITE.test(text) && !wantsSqlBlock) {
    const first = tables[0];
    const t1 = call('describe', 'describe_table', { table: first });
    await waiting(500);
    done(t1, `${schema[first].length} columns · 48,210 rows`);
    const countSql = "SELECT count(*) FROM public.orders\nWHERE status = 'cancelled' AND created_at < now() - interval '90 days';";
    const c = call('read', 'run_read_query', { sql: countSql });
    await waiting(600);
    done(c, '1 row · 58 ms', { result: { rows: 1, ms: 58 }, columns: ['count'], rows: [[214]] });
    const sql = "DELETE FROM public.orders\nWHERE status = 'cancelled'\n  AND created_at < now() - interval '90 days';";
    const w = call('write', 'propose_write', { sql });
    await waiting(300);
    if (!allowWrites) {
      done(w, `Blocked: the assistant is read-only on ${conn.name}.`, { error: { code: 'write_blocked', message: `The assistant is read-only on ${conn.name}.` } });
      await stream("This would delete **214** cancelled orders older than 90 days. I can't run it because the assistant is read-only. Turn on write mode with the shield button, or copy the SQL into the editor and run it yourself.");
      return;
    }
    const info: WriteInfo = { statements: [{ kind: 'DELETE', table: 'orders' }], warnings: [] };
    emit({ type: 'write_preview', id: w, sql, info });
    const ok = await decision(script, w);
    if (script.cancelled) throw new Cancelled();
    if (ok) done(w, '214 rows changed · committed · 38 ms', { result: { rows: 214, ms: 38, committed: true } });
    else done(w, 'You denied this statement. Nothing changed.', { error: { code: 'write_denied', message: 'You denied this statement. Nothing ran.' } });
    await stream(ok ? 'Done. I deleted 214 cancelled orders inside one transaction. The change is in the audit log.' : 'OK, I did not run it. The data is unchanged.');
    return;
  }

  // An answer with a JSON code block, to exercise the Run menu in the chat.
  if (/\bjson\s+(?:block|code)\b/i.test(text)) {
    await stream('Here is the payload:\n\n```json\n{"status":"paid","orders":3}\n```\n\nCopy it or use it as a reference.');
    return;
  }
  if (wantsSqlBlock) {
    const write = /\b(?:update|delete|insert|drop|write)\b/i.test(text);
    const sql = write ? "UPDATE public.orders\nSET status = 'paid'\nWHERE id = 1;" : 'SELECT *\nFROM public.orders\nLIMIT 10;';
    const tail = write ? 'Use Insert into editor to review it before running.' : 'Pick Run to execute it.';
    await stream(`Here is the query:\n\n\`\`\`sql\n${sql}\n\`\`\`\n\n${tail}`);
    return;
  }

  // A table named in the message ("table notes", "from notes", "from table notes"), else any table name in the text.
  const named = /\b(?:from|table)\s+(?:table\s+)?[`"']?(\w+)/i.exec(text)?.[1];
  const findTable = (name: string) => tables.find((t) => t.toLowerCase() === name.toLowerCase());
  let target = named ? findTable(named) : tables.find((t) => new RegExp(`\\b${t}\\b`, 'i').test(text));

  if (!named && schema.orders && /\b(chart|graph|plot|report)\b/i.test(text)) {
    const q = call('chart', 'render_chart', { sql: 'SELECT status, sum(total) AS revenue\nFROM public.orders\nGROUP BY status\nORDER BY revenue DESC;' });
    await waiting(800);
    emit({
      type: 'chart',
      id: q,
      spec: { type: 'bar', title: 'Revenue by order status', x: 'status', y: ['revenue'], stacked: false },
      columns: ['status', 'revenue'],
      rows: [['paid', 1843210.5], ['shipped', 1290442.1], ['pending', 402118.0], ['cancelled', 0], ['refunded', -98220.4]],
      truncated: false,
    });
    done(q, 'Bar chart · 5 rows · 84 ms', { result: { rows: 5, ms: 84 } });
    const m = call('chart', 'render_chart', { sql: "SELECT date_trunc('month', created_at) AS month, sum(total) AS revenue, count(*) AS orders\nFROM public.orders\nGROUP BY 1\nORDER BY 1;" });
    await waiting(800);
    emit({
      type: 'chart',
      id: m,
      spec: { type: 'line', title: 'Monthly revenue', x: 'month', y: ['revenue'], stacked: false },
      columns: ['month', 'revenue'],
      rows: [['2026-05', 512300], ['2026-06', 548120], ['2026-07', 601880], ['2026-08', 587400], ['2026-09', 642950], ['2026-10', 695210]],
      truncated: false,
    });
    done(m, 'Line chart · 6 rows · 61 ms', { result: { rows: 6, ms: 61 } });
    await stream(
      '# Orders revenue report\n\nRevenue grew in five of the last six months. **Paid** orders bring most of the revenue.\n\n## Revenue by status\n\n| Status | Revenue |\n|---|---:|\n| paid | $1,843,210 |\n| shipped | $1,290,442 |\n| pending | $402,118 |\n| refunded | −$98,220 |\n\n## Findings\n\n- October is the best month so far ($695K).\n- Refunds remove about 2.5% of gross revenue.\n- About 8% of orders are cancelled.',
    );
    return;
  }

  if (!target && !named && schema.orders && /\b(orders?|revenue)\b/i.test(text)) {
    const t1 = call('describe', 'describe_table', { table: 'orders' });
    await waiting(500);
    done(t1, `${schema.orders.length} columns · 48,210 rows`);
    const q = call('read', 'run_read_query', { sql: 'SELECT status, count(*) AS orders, sum(total) AS revenue\nFROM public.orders\nGROUP BY status\nORDER BY revenue DESC;' });
    await waiting(800);
    const columns = ['status', 'orders', 'revenue'];
    const rows: Row[] = [
      { status: 'paid', orders: 21480, revenue: 1843210.5 },
      { status: 'shipped', orders: 15120, revenue: 1290442.1 },
      { status: 'pending', orders: 6230, revenue: 402118.0 },
      { status: 'cancelled', orders: 3910, revenue: 0 },
      { status: 'refunded', orders: 1470, revenue: -98220.4 },
    ];
    done(q, '5 rows · 84 ms', { result: { rows: 5, ms: 84 }, columns, rows: rowsOf(rows, columns) });
    await stream('**Paid** orders bring the most revenue ($1.84M from 21,480 orders), then **shipped** ($1.29M). About 8% of orders are cancelled, and refunds remove about $98K.');
    return;
  }

  if (!target) {
    await waiting(400);
    await stream(named ? `I can't find a table named **${named}** on ${conn.name}. The tables are: ${tables.join(', ')}.` : `Which table on ${conn.name} should I look at? The tables are: ${tables.join(', ')}.`);
    return;
  }

  const data = tableData(schema, target);
  const total = data.base.length + data.added.length;
  await stream('I’ll check the table schema first.', 'reasoning');
  const t1 = call('describe', 'describe_table', { table: target });
  await waiting(500);
  done(t1, `${schema[target].length} columns · ${total.toLocaleString()} rows`);
  await stream('Now I’ll read a small sample.');
  const q = call('read', 'run_read_query', { sql: `SELECT *\nFROM ${nsOf(conn.engine, target)}.${target}\nLIMIT 5;` });
  await waiting(700);
  const rows = data.base.slice(0, 5);
  done(q, `${rows.length} row${rows.length === 1 ? '' : 's'} · 41 ms`, { result: { rows: rows.length, ms: 41 }, columns: schema[target], rows: rowsOf(rows, schema[target]) });
  await stream('The sample is ready to summarize.', 'reasoning');
  await stream(`Here are the first ${rows.length} rows of **${target}** (${total.toLocaleString()} rows in total). Ask me to filter or summarize them.`);
}

/** Waits for Approve/Deny; a cancel resolves it as a denial. */
function decision(script: Script, id: string): Promise<boolean> {
  if (script.cancelled) return Promise.resolve(false);
  return new Promise<boolean>((resolve) => {
    script.pending = { id, resolve };
  });
}
