/** Validation and completion helpers for the Compass-style MongoDB filter document. */

export const MONGO_FILTER_OPERATORS = [
  '$eq', '$ne', '$gt', '$gte', '$lt', '$lte', '$in', '$nin',
  '$and', '$or', '$nor', '$not', '$exists', '$type', '$regex', '$options',
  '$elemMatch', '$size', '$all', '$mod', '$bitsAllSet', '$bitsAnySet', '$bitsAllClear', '$bitsAnyClear',
  '$geoWithin', '$geoIntersects', '$near', '$nearSphere', '$maxDistance', '$minDistance',
];

/** A schema path as a query path: array element documents `items[].sku` match as `items.sku`. */
export const mongoQueryPath = (path: string) => path.replace(/\[\]/g, '');

const HELPERS = new Set([
  'ObjectId', 'ISODate', 'Date', 'NumberInt', 'Int32', 'NumberLong', 'Long', 'NumberDecimal', 'Decimal128',
  'Double', 'UUID', 'BinData', 'Timestamp', 'RegExp', 'Code', 'MinKey', 'MaxKey',
]);

const JS_WORDS = new Set([
  'function', 'var', 'let', 'const', 'return', 'if', 'else', 'for', 'while', 'do', 'switch', 'case', 'break',
  'continue', 'class', 'this', 'typeof', 'instanceof', 'delete', 'throw', 'try', 'catch', 'finally', 'async',
  'await', 'yield', 'import', 'export', 'print', 'printjson',
]);

type Tok =
  | { t: 'ident' | 'num' | 'str'; v: string; at: number }
  | { t: 'regex'; at: number }
  | { t: 'punct'; v: string; at: number }
  | { t: 'eof'; at: number };

type Lexed = { toks: Tok[] } | { error: string; at: number };

const asciiPunct = new Set('{}[](),:;+-'.split(''));
const identStart = (c: string) => c === '_' || c === '$' || /\p{L}/u.test(c);
const identPart = (c: string) => c === '_' || c === '$' || /\p{L}|\p{N}/u.test(c);

function lex(src: string): Lexed {
  const toks: Tok[] = [];
  let i = 0;
  let valueStart = true;
  const fail = (error: string, at: number): Lexed => ({ error, at });

  while (i < src.length) {
    const start = i;
    const c = src[i];
    if (/\s/.test(c)) {
      i++;
      continue;
    }
    if (c === '/' && src[i + 1] === '/') {
      while (i < src.length && src[i] !== '\n') i++;
      continue;
    }
    if (c === '/' && src[i + 1] === '*') {
      const end = src.indexOf('*/', i + 2);
      if (end < 0) return fail('Unterminated comment', start);
      i = end + 2;
      continue;
    }
    if (c === '"' || c === "'" || c === '`') {
      const quote = c;
      i++;
      let ok = false;
      while (i < src.length) {
        const ch = src[i];
        if (ch === quote) {
          ok = true;
          i++;
          break;
        }
        if (ch === '\n' && quote !== '`') return fail('Unterminated string', start);
        if (ch === '$' && quote === '`' && src[i + 1] === '{') return fail('Template placeholders (${…}) are not supported', i);
        if (ch === '\\') {
          i++;
          if (i >= src.length) return fail('Unterminated string', start);
          // One escaped code point; the exact escape value does not matter for validation.
          i += src[i] === '\r' && src[i + 1] === '\n' ? 2 : 1;
        } else {
          i += ch.length;
        }
      }
      if (!ok) return fail('Unterminated string', start);
      toks.push({ t: 'str', v: src.slice(start, i), at: start });
      valueStart = false;
      continue;
    }
    if (c === '/' && valueStart) {
      i++;
      let cls = false;
      let ok = false;
      while (i < src.length) {
        const ch = src[i];
        if (ch === '\n') return fail('Unterminated regular expression', start);
        if (ch === '\\') i += 2;
        else if (ch === '[') { cls = true; i++; }
        else if (ch === ']') { cls = false; i++; }
        else if (ch === '/' && !cls) { ok = true; i++; break; }
        else i += ch.length;
      }
      if (!ok) return fail('Unterminated regular expression', start);
      while (i < src.length && /[a-z]/i.test(src[i])) {
        if (!'imsux'.includes(src[i])) return fail(`Unsupported regular expression flag: ${src[i]}`, i);
        i++;
      }
      toks.push({ t: 'regex', at: start });
      valueStart = false;
      continue;
    }
    if (/\d/.test(c) || (c === '.' && /\d/.test(src[i + 1] ?? ''))) {
      if (c === '0' && /[xX]/.test(src[i + 1] ?? '')) {
        i += 2;
        while (i < src.length && /[0-9a-f]/i.test(src[i])) i++;
      } else {
        while (i < src.length && /[\d.]/.test(src[i])) i++;
        if (/[eE]/.test(src[i] ?? '')) {
          i++;
          if (/[+-]/.test(src[i] ?? '')) i++;
          while (i < src.length && /\d/.test(src[i])) i++;
        }
      }
      toks.push({ t: 'num', v: src.slice(start, i), at: start });
      valueStart = false;
      continue;
    }
    if (identStart(c)) {
      i++;
      while (i < src.length && identPart(src[i])) i++;
      toks.push({ t: 'ident', v: src.slice(start, i), at: start });
      valueStart = false;
      continue;
    }
    if (asciiPunct.has(c)) {
      toks.push({ t: 'punct', v: c, at: start });
      valueStart = c === '(' || c === ',' || c === ':' || c === '[' || c === '{' || c === ';';
      i++;
      continue;
    }
    return fail(`Unexpected character '${c}'`, start);
  }
  toks.push({ t: 'eof', at: src.length });
  return { toks };
}

class Parser {
  pos = 0;
  readonly src: string;
  readonly toks: Tok[];

  constructor(src: string, toks: Tok[]) {
    this.src = src;
    this.toks = toks;
  }

  peek() {
    return this.toks[this.pos];
  }

  next() {
    const t = this.peek();
    if (t.t !== 'eof') this.pos++;
    return t;
  }

  punct(c: string) {
    const t = this.peek();
    return t.t === 'punct' && 'v' in t && t.v === c;
  }

  eat(c: string) {
    if (!this.punct(c)) return false;
    this.pos++;
    return true;
  }

  expect(c: string) {
    if (!this.eat(c)) throw this.error(`Expected '${c}'`);
  }

  error(message: string, t = this.peek()) {
    const before = this.src.slice(0, t.at);
    const line = before.split('\n').length;
    const column = before.length - (before.lastIndexOf('\n') + 1) + 1;
    return { message: `${message} at line ${line}, column ${column}` };
  }

  object() {
    this.expect('{');
    while (!this.eat('}')) {
      const t = this.next();
      if (t.t === 'eof') throw this.error('Unterminated document', t);
      if (!(t.t === 'ident' || t.t === 'str' || t.t === 'num')) {
        if (t.t === 'punct' && 'v' in t && t.v === '[') {
          const key = this.next();
          if (key.t !== 'str') throw this.error('Expected a string key', key);
          this.expect(']');
        } else {
          throw this.error('Expected a field name', t);
        }
      }
      this.expect(':');
      this.value();
      if (this.eat(',')) continue;
      this.expect('}');
      return;
    }
  }

  array() {
    this.expect('[');
    while (!this.eat(']')) {
      if (this.peek().t === 'eof') throw this.error('Unterminated array');
      this.value();
      if (this.eat(',')) continue;
      this.expect(']');
      return;
    }
  }

  call() {
    this.expect('(');
    while (!this.eat(')')) {
      if (this.peek().t === 'eof') throw this.error('Unterminated call');
      this.value();
      if (this.eat(',')) continue;
      this.expect(')');
      return;
    }
  }

  number(raw: string, at: number) {
    const ok = /^0[xX][0-9a-fA-F]+$/.test(raw) || /^(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(raw);
    if (!ok) throw this.error(`Invalid number: ${raw}`, { t: 'num', v: raw, at });
  }

  value() {
    const t = this.next();
    if (t.t === 'eof') throw this.error('Expected a value', t);
    if (t.t === 'str' || t.t === 'regex') return;
    if (t.t === 'num') {
      this.number(t.v, t.at);
      return;
    }
    if (t.t === 'punct' && 'v' in t) {
      if (t.v === '{') {
        this.pos--;
        this.object();
        return;
      }
      if (t.v === '[') {
        this.pos--;
        this.array();
        return;
      }
      if (t.v === '-' || t.v === '+') {
        const n = this.next();
        if (n.t === 'num') {
          this.number(n.v, n.at);
          return;
        }
        if (n.t === 'ident' && n.v === 'Infinity') return;
        throw this.error('Expected a number', n);
      }
    }
    if (t.t === 'ident') {
      if (['true', 'false', 'null', 'undefined', 'Infinity', 'NaN'].includes(t.v)) return;
      if (JS_WORDS.has(t.v)) throw this.error(`JavaScript is not supported: '${t.v}'`, t);
      if (t.v === 'new') {
        const n = this.next();
        if (n.t !== 'ident') throw this.error('Expected a type such as Date or ObjectId', n);
        this.helper(n.v, n.at, true);
        return;
      }
      this.helper(t.v, t.at, false);
      return;
    }
    throw this.error('Expected a value', t);
  }

  helper(name: string, at: number, withNew: boolean) {
    if (!HELPERS.has(name)) throw this.error(`Unknown function: ${name}`, { t: 'ident', v: name, at });
    if (!this.punct('(')) throw this.error(`Expected '(' after ${name}`, { t: 'ident', v: name, at });
    if (name === 'Date' && !withNew) throw this.error('Use new Date(…) or ISODate(…)', { t: 'ident', v: name, at });
    this.call();
  }
}


export type MongoFilterValidation = { valid: true } | { valid: false; error: string };

/**
 * Validates the mongosh object literal that the filter bar accepts. This mirrors the backend grammar:
 * documents, arrays, strings, numbers, regular expressions, comments, trailing commas and BSON helper
 * calls are valid; arbitrary JavaScript expressions are not.
 */
export function validateMongoFilter(text: string): MongoFilterValidation {
  const src = text.trim();
  if (!src) return { valid: true };
  const out = lex(src);
  if ('error' in out) return { valid: false, error: out.error };
  try {
    const p = new Parser(src, out.toks);
    p.object();
    if (p.peek().t !== 'eof') throw p.error('Expected the end of the filter');
    return { valid: true };
  } catch (e) {
    const error = e && typeof e === 'object' && 'message' in e ? String(e.message) : 'Invalid filter document';
    return { valid: false, error };
  }
}

/** Empty input and `{}` are Compass's empty filter. */
export const emptyMongoFilter = (text: string) => !text.trim() || text.trim() === '{}';

/** The filter document to pass to `find`, or an error that names what is not valid mongosh literal syntax. */
export function mongoFilterDocument(text: string): string {
  const doc = text.trim() || '{}';
  const valid = validateMongoFilter(doc);
  if (!valid.valid) throw new Error(`Invalid MongoDB filter: ${valid.error}`);
  return doc;
}
