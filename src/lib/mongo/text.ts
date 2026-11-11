/** Mongosh statement splitting that skips strings, comments and regex literals. */

/**
 * Statements of a mongosh script: a top-level `;` ends one, and a line that begins (after
 * whitespace) with `db.`, `show ` or `use ` starts one, while brackets balance. Strings, comments
 * and regex literals never split. Ranges exclude surrounding whitespace and the `;`; blank ones
 * are dropped.
 */
export function mongoStatementRanges(doc: string): { from: number; to: number }[] {
  const out: { from: number; to: number }[] = [];
  let start = 0; // start of the open statement
  let depth = 0; // () [] {} nesting
  let hasCode = false; // the open statement holds code, not only blanks and comments
  let lineStart = true; // the next code character begins a line's first token
  let regexOK = true; // a `/` here can open a regex (false right after a word, `)`, `]`, `}` or string)
  const cut = (end: number) => {
    const text = doc.slice(start, end);
    if (text.trim()) out.push({ from: start + (text.length - text.trimStart().length), to: start + text.trimEnd().length });
    start = end;
  };
  for (let i = 0; i < doc.length; i++) {
    const ch = doc[i];
    if (ch === '\n') {
      lineStart = true;
      continue;
    }
    if (/\s/.test(ch)) continue;
    // Comments are one token: their `;` never cuts and they leave `lineStart` alone, so code after
    // a leading comment still counts as the line's first token.
    if (ch === '/' && doc[i + 1] === '/') {
      const e = doc.indexOf('\n', i);
      i = e < 0 ? doc.length : e - 1; // the loop's `i++` lands on the `\n`, so the next line starts fresh
      continue;
    }
    if (ch === '/' && doc[i + 1] === '*') {
      const e = doc.indexOf('*/', i + 2);
      i = e < 0 ? doc.length : e + 1;
      continue;
    }
    // A line that begins a new `db.` / `show` / `use` statement cuts the open one before it.
    if (lineStart) {
      lineStart = false;
      if (depth === 0 && hasCode && (doc.startsWith('db.', i) || doc.startsWith('show ', i) || doc.startsWith('use ', i))) {
        cut(i);
        start = i;
        hasCode = false;
      }
    }
    // Strings are one token: they end at the first unescaped matching quote.
    if (ch === '\'' || ch === '"' || ch === '`') {
      let j = i + 1;
      while (j < doc.length && doc[j] !== ch) j += doc[j] === '\\' ? 2 : 1;
      i = j;
      hasCode = true;
      continue;
    }
    // A regex literal: skip to the closing unescaped `/`, honoring escapes and `[...]` classes.
    if (ch === '/' && regexOK) {
      let j = i + 1;
      let cls = false;
      while (j < doc.length) {
        const c = doc[j];
        if (c === '\\') { j += 2; continue; }
        if (c === '[') cls = true;
        else if (c === ']') cls = false;
        else if (c === '/' && !cls) break;
        j++;
      }
      i = j;
      hasCode = true;
      continue;
    }
    if (ch === ';' && depth === 0) {
      cut(i);
      start = i + 1;
      hasCode = false;
      regexOK = true;
      continue;
    }
    if (ch === '(' || ch === '[' || ch === '{') depth++;
    else if (ch === ')' || ch === ']' || ch === '}') depth = Math.max(0, depth - 1);
    regexOK = !/[\w$)\]}'"`]/.test(ch);
    hasCode = true;
  }
  cut(doc.length);
  return out;
}

/**
 * `sql` after `existing` as its own mongosh statement: a blank line separates them and the
 * previous text ends with `;` when it does not already. A `db.`/`show`/`use` line start would
 * begin a statement without it; the `;` also covers other mongosh text.
 */
export function appendMongoStatement(existing: string, sql: string): string {
  if (!existing.trim()) return sql;
  const body = existing.trimEnd();
  return /;\s*$/.test(body) ? `${body}\n\n${sql}` : `${body};\n\n${sql}`;
}
