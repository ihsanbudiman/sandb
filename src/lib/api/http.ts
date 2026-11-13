import type { Transport } from './backend';
import { BRIDGE_URL } from './devConfig';
import { ApiError } from './types';
import type { ChatEvent, CommandName } from './generated/wire';

// Dev-only transport to the Rust bridge (`pnpm bridge`). The bridge only allows the origin of the
// Vite dev server; both defaults live in `devConfig.ts` and are overridden by `VITE_BRIDGE_*`.
const base = ((import.meta.env.VITE_BRIDGE_URL as string | undefined) || BRIDGE_URL).replace(/\/+$/, '');
const token = (import.meta.env.VITE_BRIDGE_TOKEN as string | undefined) ?? '';

const unreachable = (e: unknown) => new ApiError({ code: 'io', message: 'Cannot reach the dev bridge', detail: `${base}: ${e instanceof Error ? e.message : e}` });

async function body(res: Response): Promise<unknown> {
  const text = await res.text();
  if (!text) return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}

function failure(res: Response, raw: unknown): ApiError {
  if (raw && typeof raw === 'object' && 'code' in raw) return ApiError.from(raw);
  const text = typeof raw === 'string' ? raw.trim() : '';
  return new ApiError({ code: 'internal', message: text || `Bridge returned HTTP ${res.status}` });
}

export const httpTransport: Transport = {
  async call<T>(name: CommandName, args: Record<string, unknown> | Uint8Array = {}, headers?: Record<string, string>): Promise<T> {
    // `close_tab_session` runs from the page-hide cleanup; `keepalive` keeps the request alive while the page unloads.
    const keepalive = name === 'close_tab_session';
    const bytes = args instanceof Uint8Array;
    let res: Response;
    try {
      res = await fetch(`${base}/api/${name}`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': bytes ? 'application/octet-stream' : 'application/json', ...headers },
        body: bytes ? args.slice().buffer : JSON.stringify(args),
        keepalive,
      });
    } catch (e) {
      throw unreachable(e);
    }
    const raw = await body(res);
    if (!res.ok) throw failure(res, raw);
    return raw as T;
  },

  async stream(name, args, onEvent) {
    let res: Response;
    try {
      res = await fetch(`${base}/api/${name}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}`, Accept: 'text/event-stream' },
        body: JSON.stringify(args),
      });
    } catch (e) {
      throw unreachable(e);
    }
    if (!res.ok || !res.body) throw failure(res, await body(res));
    const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
    let buf = '';
    let ended = false;
    const line = (l: string) => {
      if (!l.startsWith('data:')) return;
      let e: ChatEvent;
      try {
        e = JSON.parse(l.slice(5).trim()) as ChatEvent;
      } catch {
        throw new ApiError({ code: 'ai_protocol', message: 'The dev bridge sent an event that is not valid JSON' });
      }
      onEvent(e);
      if (e.type === 'done' || e.type === 'error') ended = true;
    };
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += value;
        const lines = buf.split(/\r?\n/);
        buf = lines.pop() ?? '';
        for (const l of lines) line(l);
      }
      line(buf);
    } catch (e) {
      reader.cancel().catch(() => {});
      if (e instanceof ApiError) throw e;
      throw new ApiError({ code: 'io', message: 'The connection to the dev bridge stopped', detail: e instanceof Error ? e.message : String(e) });
    } finally {
      reader.releaseLock();
    }
    if (!ended) throw new ApiError({ code: 'ai_protocol', message: 'The reply stopped before it was complete' });
  },
};
