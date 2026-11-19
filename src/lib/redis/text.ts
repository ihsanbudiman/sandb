const encoder = new TextEncoder();
const decoder = new TextDecoder('latin1');
const separator = (c: string | undefined) => c === ' ' || c === '\t' || c === '\r' || c === '\n';

/** The offset after the matching quote, or -1. `from` is inside the quoted argument. */
export function redisQuoteEnd(text: string, from: number, quote: string): number {
  for (let i = from; i < text.length; i++) {
    if (text[i] === quote) return i + 1;
    if (text[i] === '\\' && (quote === '"' || text[i + 1] === "'")) i++;
  }
  return -1;
}

function syntax(doc: string, offset: number, message: string): never {
  const lines = doc.slice(0, offset).split('\n');
  throw new Error(`Line ${lines.length}, column ${Array.from(lines[lines.length - 1]).length + 1}: ${message}`);
}

function argumentEnd(doc: string, from: number, strict: boolean): number {
  let to = from;
  const quote = doc[from];
  if (quote === '"' || quote === "'") {
    to = redisQuoteEnd(doc, from + 1, quote);
    if (to < 0) {
      if (strict) syntax(doc, from, 'Unclosed quote');
      return doc.length;
    }
    if (strict && to < doc.length && !separator(doc[to])) syntax(doc, to, 'Closing quote must be followed by a space');
  }
  while (to < doc.length && !separator(doc[to])) to++;
  return to;
}

type Range = { from: number; to: number };

/** Visit source offsets without decoding arguments. In the editor, keep unfinished commands selectable. */
function scan(doc: string, statement: (from: number, to: number) => void, arg?: (from: number, to: number, index: number) => void, strict = false) {
  let from = 0, to = 0, count = 0;
  for (let i = 0; i < doc.length;) {
    const c = doc[i];
    if (c === '\n') {
      if (count) statement(from, to);
      count = 0;
      i++;
    } else if (separator(c)) {
      i++;
    } else if (!count && c === '#') {
      const end = doc.indexOf('\n', i + 1);
      i = end < 0 ? doc.length : end + 1;
    } else {
      if (!count) from = i;
      to = argumentEnd(doc, i, strict);
      arg?.(i, to, count);
      count++;
      i = to;
    }
  }
  if (count) statement(from, to);
}

export function redisStatementRanges(doc: string): Range[] {
  const ranges: Range[] = [];
  scan(doc, (from, to) => ranges.push({ from, to }));
  return ranges;
}

const hex = (c: number) => c >= 48 && c <= 57 ? c - 48 : (c | 32) >= 97 && (c | 32) <= 102 ? (c | 32) - 87 : -1;
const escapes = new Map([[110, 10], [114, 13], [116, 9], [98, 8], [97, 7]]); // n, r, t, b, a

function decodeArg(arg: string): Uint8Array {
  const quote = arg[0];
  if (quote !== '"' && quote !== "'") return encoder.encode(arg);
  const bytes = encoder.encode(arg.slice(1, -1));
  let n = 0;
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if (b === 92 && (quote === '"' || bytes[i + 1] === 39)) {
      const next = bytes[++i];
      if (quote === '"' && next === 120 && i + 2 < bytes.length) {
        const hi = hex(bytes[i + 1]), lo = hex(bytes[i + 2]);
        if (hi >= 0 && lo >= 0) {
          bytes[n++] = hi * 16 + lo;
          i += 2;
          continue;
        }
      }
      bytes[n++] = quote === '"' ? escapes.get(next) ?? next : next;
    } else {
      bytes[n++] = b;
    }
  }
  return bytes.subarray(0, n);
}

/** Strict decoding for the local demo backend, including redis-cli's byte escapes. */
export function parseRedisStatements(doc: string): { text: string; args: Uint8Array[] }[] {
  const statements: { text: string; args: Uint8Array[] }[] = [];
  let args: Uint8Array[] = [];
  scan(doc, (from, to) => {
    statements.push({ text: doc.slice(from, to), args });
    args = [];
  }, (from, to) => args.push(decodeArg(doc.slice(from, to))), true);
  return statements;
}

/** Completion replaces the active argument, including an already-typed closing quote. */
export function redisCompletionRange(doc: string, pos: number): (Range & { command: boolean }) | null {
  const before = doc.slice(0, pos);
  let from = pos, end = 0, index = 0;
  scan(before, () => {}, (start, to, argIndex) => {
    from = start;
    end = to;
    index = argIndex;
  });
  if (end === pos && from < pos) return { from, to: argumentEnd(doc, from, false), command: index === 0 };
  const rest = before.slice(end);
  const line = rest.slice(rest.lastIndexOf('\n') + 1);
  if (/^[ \t\r]*#/.test(line)) return null;
  return { from: pos, to: pos, command: !end || rest.includes('\n') };
}

export function appendRedisStatement(existing: string, cmd: string): string {
  return existing && !existing.endsWith('\n') ? `${existing}\n${cmd}` : existing + cmd;
}

/** Quote new input only. Names from the server are already canonical argument tokens. */
export function quoteRedisArg(input: string | Uint8Array): string {
  const bare = typeof input === 'string' ? input : decoder.decode(input);
  if (bare && !/[^A-Za-z0-9_:.+*/@{}\[\]$!?=<>%^&|~,-]/.test(bare)) return bare;
  const bytes = typeof input === 'string' ? encoder.encode(input) : input;
  let out = '"';
  for (const b of bytes) {
    if (b === 34) out += '\\"';
    else if (b === 92) out += '\\\\';
    else if (b === 10) out += '\\n';
    else if (b === 13) out += '\\r';
    else if (b === 9) out += '\\t';
    else if (b >= 0x20 && b <= 0x7e) out += String.fromCharCode(b);
    else out += `\\x${b.toString(16).padStart(2, '0')}`;
  }
  return out + '"';
}
