import type { CellValue, ChartSpec, Row, ToolKind, WriteInfo } from './api';
import type { Source } from './connections.svelte';

/** `denied`: the user denied a proposed write, or the guard refused it. `failed`: the tool or the database returned an error. */
export type ToolStatus = 'running' | 'done' | 'blocked' | 'awaiting' | 'approved' | 'denied' | 'failed';

/**
 * `tool`: what the step does, from the backend (the tool's kind, never its name). `id`: the backend's tool call id (live replies only).
 * `args`: the arguments the model sent. `ms`: time from the call to its result. `truncated`: more rows exist than `rows` holds.
 * `error`: the error code of a failed step. `source`: the database the step ran on (live replies only).
 */
export type ToolPart = {
  kind: 'tool';
  tool: ToolKind;
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

export type ErrorPart = { kind: 'error'; text: string };

/** A chart of a `render_chart` read. `rows` are arrays in `columns` order. `truncated`: the read had more rows than the chart keeps. */
export type ChartPart = { kind: 'chart'; spec: ChartSpec; columns: string[]; rows: CellValue[][]; truncated: boolean };

export type Part = { kind: 'text'; text: string } | ToolPart | ErrorPart | ChartPart;

/**
 * `started`/`ended`: when the assistant reply was sent and when it finished, stopped or failed (missing in chats saved before they were kept).
 * `skills`: the skills a question carried, by name (missing when it carried none); a retry sends the same ones.
 */
export interface Msg {
  role: 'user' | 'assistant';
  parts: Part[];
  started?: number;
  ended?: number;
  skills?: string[];
}

export interface Session {
  id: string;
  sources: Source[];
  title: string;
  updated: number;
  messages: Msg[];
  /** Skills the user picked for this chat, by name. Missing in chats saved before skills existed; a new chat starts with none. */
  skills?: string[];
  /** The schedule that started this chat; missing for chats the user started. */
  scheduleId?: string;
}
