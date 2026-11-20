import type { Row } from './api';
import { connOf, dbOf, isSql, type Conn, type Source } from './connections.svelte';
import type { Msg, Part, ToolPart } from './chatModel';
import { isDescribeTool, isReadTool, mongoStep, redisStep } from './chatTools';
import { skillTitle } from './skills';

/** Consecutive tool steps of one reply form one timeline; `at` is the index of each part in the message. */
export type Block = { kind: 'part'; p: Exclude<Part, ToolPart>; at: number } | { kind: 'steps'; steps: { p: ToolPart; at: number }[] };

export function blocks(parts: Part[], skip?: (p: Part, at: number) => boolean): Block[] {
  const out: Block[] = [];
  parts.forEach((p, at) => {
    if (skip?.(p, at)) return;
    const last = out.at(-1);
    if (p.kind !== 'tool') out.push({ kind: 'part', p, at });
    else if (last?.kind === 'steps') last.steps.push({ p, at });
    else out.push({ kind: 'steps', steps: [{ p, at }] });
  });
  return out;
}

/** The step's target: a skill's title, a described table, or the query's first line. */
export const stepTarget = (p: ToolPart) => (p.tool === 'skill' ? skillTitle(typeof p.args?.name === 'string' ? p.args.name : '') : isDescribeTool(p) ? p.sql : p.sql?.trim().split('\n')[0]) ?? '';

export const duration = (ms?: number) => (ms === undefined ? '' : ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`);

export const replyMs = (m: Msg) => (m.started !== undefined && m.ended !== undefined ? m.ended - m.started : undefined);

export const toolMs = (steps: ToolPart[]) => (steps.some((s) => s.ms !== undefined) ? steps.reduce((n, s) => n + (s.ms ?? 0), 0) : undefined);

export function lastQuery(parts: Part[]) {
  for (let i = parts.length - 1; i >= 0; i--) {
    const x = parts[i];
    if (x.kind === 'tool' && isReadTool(x) && x.status === 'done' && x.sql) return { p: x, sql: x.sql };
  }
}

export function numeric(rows: Row[]) {
  const out: Record<string, boolean> = {};
  for (const c of Object.keys(rows[0])) out[c] = typeof rows.find((r) => r[c] !== null)?.[c] === 'number';
  return out;
}

/** The model's arguments; steps saved before they were kept fall back to the query text or table name. */
export const argsOf = (p: ToolPart) => p.args ?? (p.sql === undefined ? {} : isDescribeTool(p) ? { table: p.sql } : redisStep(p) ? { command: p.sql } : { sql: p.sql });

export const busy = (parts: Part[], pending: boolean) => parts.some((x) => x.kind === 'tool' && (x.status === 'running' || (x.status === 'awaiting' && pending)));

export const chatDatabaseLabel = (c: Conn | undefined, database: string) => c?.engine === 'redis' ? `db${database}` : database;

export function sourceLabel(s: Source): string {
  const c = connOf(s);
  if (!c) return 'Deleted connection';
  return c.engine === 'sqlite' ? c.name : `${c.name} / ${chatDatabaseLabel(c, dbOf(s, c))}`;
}

/** How long ago `t` was. `year`: the short saved-query date, with the year once the entry is old. */
export function ago(t: number, opts: { year?: boolean } = {}): string {
  const minutes = Math.round((Date.now() - t) / 60000);
  if (minutes < 1) return 'just now';
  if (minutes < 60) return `${minutes} min ago`;
  if (minutes < 1440) return `${Math.round(minutes / 60)} h ago`;
  if (!opts.year) return new Date(t).toLocaleDateString();
  return new Date(t).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: minutes > 300 * 1440 ? 'numeric' : undefined });
}

export const mongoSource = (s: Source | undefined) => (s ? connOf(s)?.engine === 'mongo' : false);

/** The database of the reply: its last data step, else the only chat database, else the active editor. */
export function replySource(m: Msg, scope: Source[], active?: Source): Source | undefined {
  for (let i = m.parts.length - 1; i >= 0; i--) {
    const p = m.parts[i];
    if (p.kind === 'tool' && p.source) return p.source;
  }
  if (scope.length === 1) return scope[0];
  return active;
}

/** True when the reply or its chat scope involves MongoDB: ```javascript fences may hold mongosh. */
export const mongoInReply = (m: Msg, scope: Source[]) => scope.some((s) => mongoSource(s)) || m.parts.some((p) => p.kind === 'tool' && mongoStep(p));

/** The source a code block runs on: the reply's last step of that engine, else a scope connection of it, else the reply's usual source. */
export function fenceSource(m: Msg, scope: Source[], active: Source | undefined, language: 'sql' | 'mongo' | 'redis'): Source | undefined {
  const fits = (s: Source) => {
    const c = connOf(s);
    return !!c && (language === 'sql' ? isSql(c) : c.engine === language);
  };
  for (let i = m.parts.length - 1; i >= 0; i--) {
    const p = m.parts[i];
    if (p.kind === 'tool' && p.source && fits(p.source)) return p.source;
  }
  const scoped = scope.find(fits);
  if (scoped) return scoped;
  if (language === 'redis') return active && fits(active) ? active : undefined;
  const fallback = replySource(m, scope, active);
  return fallback && connOf(fallback)?.engine === 'redis' ? (active && fits(active) ? active : undefined) : fallback;
}
