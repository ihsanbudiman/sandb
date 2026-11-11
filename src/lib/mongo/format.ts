/**
 * Mongosh beautifier for the editor. It only changes whitespace and drops trailing commas: every
 * other token (numbers such as `1.0`, strings, regex literals, comments) stays as written, because
 * sandb's own parser reads the text and `1.0` and `1` can be different BSON types.
 */

import { mongoStatementRanges } from './text';

const WIDTH = 80;
const INDENT = 2;

type Kind = 'word' | 'str' | 'op' | 'open' | 'close' | 'comma' | 'colon' | 'dot' | 'line' | 'block';
/** `space`: whitespace before the token in the source; `nl`: a line break among it. */
type Tok = { text: string; kind: Kind; space: boolean; nl: boolean };
type Atom = { t: 'atom'; tok: Tok };
type Group = { t: 'group'; open: string; close: string; els: El[]; space: boolean };
type Node = Atom | Group;
/** One comma-separated element of a group; `trailing` holds line comments after it. */
type El = { nodes: Node[]; trailing: string[] };

const CLOSE: Record<string, string> = { '(': ')', '[': ']', '{': '}' };

function tokenize(src: string): Tok[] {
  const out: Tok[] = [];
  let space = false;
  let nl = false;
  let regexOK = true; // a `/` here opens a regex literal, as in `mongoStatementRanges`
  const push = (text: string, kind: Kind) => {
    out.push({ text, kind, space, nl });
    space = nl = false;
    if (kind !== 'line' && kind !== 'block') regexOK = !(kind === 'word' || kind === 'str' || kind === 'close');
  };
  for (let i = 0; i < src.length; ) {
    const ch = src[i];
    if (/\s/.test(ch)) {
      space = true;
      if (ch === '\n') nl = true;
      i++;
    } else if (src.startsWith('//', i)) {
      const e = src.indexOf('\n', i);
      const end = e < 0 ? src.length : e;
      push(src.slice(i, end).trimEnd(), 'line');
      i = end;
    } else if (src.startsWith('/*', i)) {
      const e = src.indexOf('*/', i + 2);
      if (e < 0) throw new Error('Unclosed /* comment');
      push(src.slice(i, e + 2), 'block');
      i = e + 2;
    } else if (ch === '"' || ch === "'" || ch === '`') {
      let j = i + 1;
      while (j < src.length && src[j] !== ch) j += src[j] === '\\' ? 2 : 1;
      if (j >= src.length) throw new Error(`Unclosed string ${ch}`);
      push(src.slice(i, j + 1), 'str');
      i = j + 1;
    } else if (ch === '/' && regexOK) {
      let j = i + 1;
      let cls = false;
      while (j < src.length && src[j] !== '\n') {
        const c = src[j];
        if (c === '\\') { j += 2; continue; }
        if (c === '[') cls = true;
        else if (c === ']') cls = false;
        else if (c === '/' && !cls) break;
        j++;
      }
      if (src[j] !== '/') throw new Error('Unclosed regular expression');
      j++;
      while (j < src.length && /[a-z]/i.test(src[j])) j++; // flags
      push(src.slice(i, j), 'str');
      i = j;
    } else if (ch in CLOSE) {
      push(ch, 'open');
      i++;
    } else if (ch === ')' || ch === ']' || ch === '}') {
      push(ch, 'close');
      i++;
    } else if (ch === ',' || ch === ':' || ch === ';') {
      push(ch, ch === ',' ? 'comma' : ch === ':' ? 'colon' : 'op');
      i++;
    } else {
      // Identifiers and numbers keep their dots (`db.users.find`, `1.5`); a lone `.` chains calls.
      const m = /^(?:[\w$]+(?:\.[\w$]+)*|\.[0-9][\w]*)/.exec(src.slice(i));
      if (m) {
        push(m[0], 'word');
        i += m[0].length;
      } else if (ch === '.' && !src.startsWith('...', i)) {
        push('.', 'dot');
        i++;
      } else {
        const op = /^[^\w\s$'"`()[\]{},:;.]+|^\.\.\./.exec(src.slice(i))![0];
        push(op, 'op');
        i += op.length;
      }
    }
  }
  return out;
}

function parse(toks: Tok[]): Node[] {
  let i = 0;
  const node = (tk: Tok): Node => (tk.kind === 'open' ? group(tk) : { t: 'atom', tok: tk });
  function group(open: Tok): Group {
    const close = CLOSE[open.text];
    const els: El[] = [];
    let cur: El = { nodes: [], trailing: [] };
    for (;;) {
      const tk = toks[i++];
      if (!tk) throw new Error(`Missing ${close}`);
      if (tk.kind === 'close') {
        if (tk.text !== close) throw new Error(`Expected ${close} but found ${tk.text}`);
        if (cur.nodes.length) els.push(cur);
        break;
      }
      if (tk.kind === 'comma') {
        els.push(cur);
        cur = { nodes: [], trailing: [] };
        // A line comment on the comma's line belongs to the element before it.
        while (toks[i]?.kind === 'line' && !toks[i].nl) els[els.length - 1].trailing.push(toks[i++].text);
        continue;
      }
      cur.nodes.push(node(tk));
    }
    // Line comments that end an element print after its comma, so the comma is not commented out.
    for (const e of els) {
      for (let last = e.nodes.at(-1); last?.t === 'atom' && last.tok.kind === 'line'; last = e.nodes.at(-1)) {
        e.trailing.unshift(last.tok.text);
        e.nodes.pop();
      }
    }
    return { t: 'group', open: open.text, close, els, space: open.space };
  }
  const top: Node[] = [];
  while (i < toks.length) {
    const tk = toks[i++];
    if (tk.kind === 'close') throw new Error(`Unexpected ${tk.text}`);
    top.push(node(tk));
  }
  return top;
}

/** A node that cannot print on one line: a line comment, or a block comment over lines. */
function hard(n: Node): boolean {
  if (n.t === 'atom') return n.tok.kind === 'line' || (n.tok.kind === 'block' && n.tok.text.includes('\n'));
  return n.els.some((e) => e.trailing.length > 0 || e.nodes.some(hard));
}

/** The separator before `n` when `prev` precedes it on the same line. */
function sep(prev: Node | undefined, n: Node): string {
  if (!prev) return '';
  if (n.t === 'atom' && (n.tok.kind === 'colon' || n.tok.kind === 'dot' || n.tok.text === ';')) return '';
  if (prev.t === 'atom' && (prev.tok.kind === 'colon' || prev.tok.kind === 'comma')) return ' ';
  if (prev.t === 'atom' && prev.tok.kind === 'dot') return '';
  // A call or index (`find(`, `a[0]`, `f()(`) hugs what it applies to; other spacing follows the source.
  if (n.t === 'group' && n.open !== '{' && (prev.t === 'group' || prev.tok.kind === 'word' || prev.tok.kind === 'str')) return '';
  return (n.t === 'group' ? n.space : n.tok.space) ? ' ' : '';
}

function flatSeq(nodes: Node[]): string {
  return nodes.map((n, i) => sep(nodes[i - 1], n) + flat(n)).join('');
}

function flat(n: Node): string {
  if (n.t === 'atom') return n.tok.text;
  const body = n.els.map((e) => flatSeq(e.nodes)).join(', ');
  return n.open === '{' && body ? `{ ${body} }` : `${n.open}${body}${n.close}`;
}

class Out {
  s = '';
  col = 0;
  write(x: string) {
    this.s += x;
    this.col += x.length;
  }
  nl(indent: number) {
    this.s = this.s.trimEnd() + '\n' + ' '.repeat(indent);
    this.col = indent;
  }
}

/** `tail`: columns that follow the sequence on its last line (a `,` or a closing bracket). */
function printSeq(nodes: Node[], indent: number, out: Out, tail = 0) {
  nodes.forEach((n, i) => {
    const prev = nodes[i - 1];
    if (prev && hard(prev) && prev.t === 'atom') out.nl(indent);
    else out.write(sep(prev, n));
    // A group fits only if the rest of the line fits too, so `find({…}).sort({…})` breaks the
    // first call, not the last.
    let rest = tail;
    for (let j = i + 1; j < nodes.length && !hard(nodes[j]); j++) rest += sep(nodes[j - 1], nodes[j]).length + flat(nodes[j]).length;
    printNode(n, indent, out, rest);
  });
}

function printNode(n: Node, indent: number, out: Out, rest: number) {
  if (n.t === 'atom') return out.write(n.tok.text);
  if (!hard(n)) {
    const f = flat(n);
    if (out.col + f.length + rest <= WIDTH) return out.write(f);
  }
  // A call with one document or array argument hugs it: `find({` … `})`.
  const only = n.els.length === 1 && !n.els[0].trailing.length && n.els[0].nodes.length === 1 ? n.els[0].nodes[0] : undefined;
  if (n.open === '(' && only?.t === 'group' && only.open !== '(') {
    out.write('(');
    printNode(only, indent, out, rest + 1);
    return out.write(')');
  }
  out.write(n.open);
  const inner = indent + INDENT;
  n.els.forEach((e, i) => {
    out.nl(inner);
    const comma = i < n.els.length - 1;
    printSeq(e.nodes, inner, out, comma ? 1 : 0);
    if (comma) out.write(',');
    e.trailing.forEach((c, j) => {
      if (j > 0) out.nl(inner);
      out.write(j === 0 && e.nodes.length ? ` ${c}` : c);
    });
  });
  out.nl(indent);
  out.write(n.close);
}

/** One statement; `semi` adds the `;` that followed it, after a line break when a line comment ends it. */
function formatStatement(src: string, semi: boolean): string {
  const nodes = parse(tokenize(src));
  if (semi) nodes.push({ t: 'atom', tok: { text: ';', kind: 'op', space: false, nl: false } });
  const out = new Out();
  printSeq(nodes, 0, out);
  return out.s.trimEnd();
}

/**
 * The mongosh text with each statement formatted: documents and arrays break over lines and indent
 * when they do not fit in 80 columns, and a blank line separates statements. A `;` after a
 * statement stays. Throws on unbalanced brackets or an unclosed string, comment or regex.
 */
export function formatMongo(text: string): string {
  return mongoStatementRanges(text)
    .map(({ from, to }) => formatStatement(text.slice(from, to), /^\s*;/.test(text.slice(to))))
    .join('\n\n');
}
