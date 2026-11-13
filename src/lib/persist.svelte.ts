/**
 * localStorage values with one writer per key: a versioned envelope, a debounced save, and the error
 * handed to the caller so a failed save can be shown instead of retried by a timer.
 */

const SAVE_MS = 250;

export interface PersistOptions<T> {
  version?: number;
  delay?: number;
  /** A failed write (full storage, private mode). */
  onError?: (e: unknown) => void;
  /** Reads a value written by another version, or before the envelope existed. */
  migrate?: (data: unknown, version: number) => T | undefined;
}

interface Envelope {
  v: number;
  data: unknown;
}

const timers: Record<string, ReturnType<typeof setTimeout>> = {};
const queued: Record<string, () => unknown> = {};

function queue(key: string, read: () => unknown, opts: PersistOptions<unknown>) {
  queued[key] = read;
  clearTimeout(timers[key]);
  timers[key] = setTimeout(() => {
    const current = queued[key];
    delete queued[key];
    try {
      const envelope: Envelope = { v: opts.version ?? 1, data: current() };
      localStorage.setItem(key, JSON.stringify(envelope));
    } catch (e) {
      opts.onError?.(e);
    }
  }, opts.delay ?? SAVE_MS);
}

export function writeStored<T>(key: string, data: T, opts: PersistOptions<T> = {}) {
  queue(key, () => data, opts as PersistOptions<unknown>);
}

const isEnvelope = (v: unknown): v is Envelope => !!v && typeof v === 'object' && 'v' in v && 'data' in v;

export function readStored<T>(key: string, initial: T, opts: PersistOptions<T> = {}): T {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(key);
  } catch {
    return initial;
  }
  if (raw === null) return initial;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    // A value stored without JSON (the old "hidden"/"shown" flags): the reader's migrate decides.
    return opts.migrate?.(raw, 0) ?? initial;
  }
  if (isEnvelope(parsed)) {
    if (parsed.v === (opts.version ?? 1)) return parsed.data as T;
    return opts.migrate?.(parsed.data, parsed.v) ?? initial;
  }
  // A value written before this envelope (or by hand): the reader's migrate decides its shape.
  return opts.migrate?.(parsed, 0) ?? (parsed as T);
}

/**
 * Saves whatever `read` returns whenever its content changes. `JSON.stringify` inside the effect is
 * what tracks every field of a store (Svelte tracks properties, not their contents); the save itself
 * serializes the value when it runs, so it stores the latest state.
 */
export function autosave<T>(key: string, read: () => T, opts: PersistOptions<T> = {}) {
  $effect.root(() => {
    $effect(() => {
      const data = read();
      JSON.stringify(data);
      queue(key, () => data, opts as PersistOptions<unknown>);
    });
  });
}

export class Pref<T> {
  value = $state() as T;
  private key: string;
  private opts: PersistOptions<T>;

  constructor(key: string, initial: T, opts: PersistOptions<T> = {}) {
    this.key = key;
    this.opts = opts;
    this.value = readStored(key, initial, opts);
  }

  set(v: T) {
    this.value = v;
    queue(this.key, () => this.value, this.opts as PersistOptions<unknown>);
  }
}
