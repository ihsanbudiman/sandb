// SQL as HTML with the editor's colors, for read-only code in the assistant. The `tok-*` classes are in app.css.
import { highlightCode, tagHighlighter, tags as t } from '@lezer/highlight';
import { MySQL, SQLite, StandardSQL, type SQLDialect } from '@codemirror/lang-sql';
import type { Engine } from './api';
import { POSTGRES } from './sql/complete';
import type { SqlEngine } from './sql/text';

const byFence: Record<string, SQLDialect> = { sql: StandardSQL, postgres: POSTGRES, postgresql: POSTGRES, pgsql: POSTGRES, psql: POSTGRES, mysql: MySQL, mariadb: MySQL, sqlite: SQLite };
const byEngine: Record<SqlEngine, SQLDialect> = { pg: POSTGRES, mysql: MySQL, sqlite: SQLite };
const looksSql = /^\s*(?:--[^\n]*\n\s*)*(?:SELECT|WITH|INSERT|UPDATE|DELETE|MERGE|CREATE|ALTER|DROP|TRUNCATE|EXPLAIN|SHOW|DESCRIBE|GRANT|REVOKE|BEGIN|COMMIT|ROLLBACK)\b/i;

const tokens = tagHighlighter([
  { tag: t.keyword, class: 'tok-kw' },
  { tag: [t.string, t.special(t.string)], class: 'tok-str' },
  { tag: t.number, class: 'tok-num' },
  { tag: t.comment, class: 'tok-cmt' },
  { tag: [t.operator, t.punctuation], class: 'tok-op' },
  { tag: t.typeName, class: 'tok-type' },
]);

const escape = (s: string) => s.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');

export function fenceDialect(lang: string | undefined, code: string): SQLDialect | undefined {
  const name = lang?.trim().split(/\s/)[0].toLowerCase();
  return name ? byFence[name] : looksSql.test(code) ? StandardSQL : undefined;
}

/** Escaped HTML of `code` with a span per SQL token; other engines get escaping only. */
export function sqlHtml(code: string, dialect: SQLDialect | Engine = StandardSQL): string {
  if (dialect === 'mongo' || dialect === 'redis') return escape(code);
  const d = typeof dialect === 'string' ? byEngine[dialect] : dialect;
  let out = '';
  highlightCode(code, d.language.parser.parse(code), tokens, (s, cls) => (out += cls ? `<span class="${cls}">${escape(s)}</span>` : escape(s)), () => (out += '\n'));
  return out;
}
