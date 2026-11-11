/** SQL text helpers shared by the editor and the column autocomplete. */

import type { Engine } from '../api';

/** The engine of the text. MongoDB never reaches these helpers; its text would split as plain SQL. */
export type Dialect = Engine;

/**
 * Cuts the text at `;` outside strings, quoted names, comments and Postgres dollar quotes, like the
 * backend split. Each range ends after its `;`; the last one may have none. With `blankLines`, a
 * blank line also ends a statement that holds code, as in DBeaver; the range then ends before it.
 */
export function statementRanges(doc: string, dialect: Dialect, blankLines = false): [number, number][] {
  const out: [number, number][] = [];
  const skipTo = (i: number, s: string) => {
    const e = doc.indexOf(s, i);
    return e < 0 ? doc.length : e + s.length - 1;
  };
  let start = 0;
  // Code (not only blanks and comments) since `start`: a blank line before any code cuts nothing.
  let hasCode = false;
  for (let i = 0; i < doc.length; i++) {
    const ch = doc[i];
    const lineComment = (ch === '-' && doc[i + 1] === '-') || (ch === '#' && dialect === 'mysql');
    const blockComment = ch === '/' && doc[i + 1] === '*';
    if (!lineComment && !blockComment && ch !== ';' && !/\s/.test(ch)) hasCode = true;
    // Stops before the `\n`, so a blank line right after the comment still counts.
    if (lineComment) i = skipTo(i, '\n') - 1;
    else if (blockComment) i = skipTo(i + 2, '*/');
    else if (ch === "'" || ch === '"' || (ch === '`' && dialect === 'mysql')) {
      // A doubled quote stays inside the string; MySQL also escapes with a backslash.
      let j = i + 1;
      while (j < doc.length && !(doc[j] === ch && doc[j + 1] !== ch)) j += doc[j] === ch || (doc[j] === '\\' && dialect === 'mysql') ? 2 : 1;
      i = j;
    } else if (ch === '$' && dialect === 'pg') {
      // $$ or $tag$ opens a dollar quote; $1 (a parameter) does not match.
      const tag = /^\$(?:[A-Za-z_]\w*)?\$/.exec(doc.slice(i, i + 64))?.[0];
      if (tag) i = skipTo(i + tag.length, tag);
    } else if (ch === ';') {
      out.push([start, i + 1]);
      start = i + 1;
      hasCode = false;
    } else if (ch === '\n' && blankLines && hasCode) {
      const end = doc.indexOf('\n', i + 1);
      if (!doc.slice(i + 1, end < 0 ? doc.length : end).trim()) {
        out.push([start, i]);
        start = i;
        hasCode = false;
      }
    }
  }
  if (start < doc.length) out.push([start, doc.length]);
  return out;
}

/**
 * `sql` after `existing` as its own statement: a `;` closes the last statement when it has none,
 * then one blank line. Comments or blanks after the last `;` need no new one.
 */
export function appendStatement(existing: string, sql: string, dialect: Dialect): string {
  if (!existing.trim()) return sql;
  const ranges = statementRanges(existing, dialect);
  const [from, to] = ranges[ranges.length - 1];
  // The last statement ends after its last character outside comments; strings count as statement text.
  let end = from;
  for (let i = from; i < to; i++) {
    const ch = existing[i];
    if ((ch === '-' && existing[i + 1] === '-') || (ch === '#' && dialect === 'mysql')) {
      const e = existing.indexOf('\n', i);
      i = e < 0 ? to : e;
    } else if (ch === '/' && existing[i + 1] === '*') {
      const e = existing.indexOf('*/', i + 2);
      i = e < 0 ? to : e + 1;
    } else if (ch === "'" || ch === '"' || (ch === '`' && dialect === 'mysql')) {
      // Skipped whole, as statementRanges does, so a `--` inside a string is text.
      let j = i + 1;
      while (j < to && !(existing[j] === ch && existing[j + 1] !== ch)) j += existing[j] === ch || (existing[j] === '\\' && dialect === 'mysql') ? 2 : 1;
      i = Math.min(j, to - 1);
      end = i + 1;
    } else if (ch === '$' && dialect === 'pg' && /^\$(?:[A-Za-z_]\w*)?\$/.test(existing.slice(i, i + 64))) {
      const tag = /^\$(?:[A-Za-z_]\w*)?\$/.exec(existing.slice(i, i + 64))![0];
      const e = existing.indexOf(tag, i + tag.length);
      i = e < 0 ? to - 1 : e + tag.length - 1;
      end = i + 1;
    } else if (!/\s/.test(ch)) {
      end = i + 1;
    }
  }
  // Only blanks and comments, or a closing `;`: nothing to terminate.
  if (end === from || existing[end - 1] === ';') return `${existing.trimEnd()}\n\n${sql}`;
  return `${existing.slice(0, end)};${existing.slice(end).trimEnd()}\n\n${sql}`;
}

/** The statement range around `pos`; undefined when the document holds none. */
export function statementRangeAt(doc: string, pos: number, dialect: Dialect): [number, number] | undefined {
  return statementRanges(doc, dialect).find(([from, to]) => pos >= from && pos <= to);
}

/** The statement around `pos`; '' when the document holds none. */
export function statementTextAt(doc: string, pos: number, dialect: Dialect): string {
  const range = statementRangeAt(doc, pos, dialect);
  return range ? doc.slice(range[0], range[1]) : '';
}

/**
 * The same text with strings and comments replaced by spaces, so a scan for `FROM` cannot see a
 * table name inside a literal or a comment. Quoted identifiers stay: they can name a table.
 */
export function maskLiterals(text: string, dialect: Dialect): string {
  const out = text.split('');
  const blank = (from: number, to: number) => {
    for (let i = from; i < to && i < out.length; i++) if (out[i] !== '\n') out[i] = ' ';
  };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if ((ch === '-' && text[i + 1] === '-') || (ch === '#' && dialect === 'mysql')) {
      const end = text.indexOf('\n', i);
      const to = end < 0 ? text.length : end;
      blank(i, to);
      i = to - 1;
    } else if (ch === '/' && text[i + 1] === '*') {
      const end = text.indexOf('*/', i + 2);
      const to = end < 0 ? text.length : end + 2;
      blank(i, to);
      i = to - 1;
    } else if (ch === "'") {
      // A doubled quote stays inside the string; MySQL also escapes with a backslash.
      let j = i + 1;
      while (j < text.length && !(text[j] === "'" && text[j + 1] !== "'")) j += text[j] === '\\' && dialect === 'mysql' ? 2 : 1;
      const to = Math.min(j + 1, text.length);
      blank(i, to);
      i = to - 1;
    } else if (ch === '$' && dialect === 'pg') {
      const tag = /^\$(?:[A-Za-z_]\w*)?\$/.exec(text.slice(i, i + 64))?.[0];
      if (tag) {
        const end = text.indexOf(tag, i + tag.length);
        const to = end < 0 ? text.length : end + tag.length;
        blank(i, to);
        i = to - 1;
      }
    }
  }
  return out.join('');
}
