/** SQL beautifier for the editor. */

import { formatDialect, postgresql, mysql, sqlite } from 'sql-formatter';
import { statementRanges, type SqlEngine } from './text';

const dialects = { pg: postgresql, mysql, sqlite } as const;

/**
 * The text with each statement formatted. The editor's split (a blank line also ends a statement)
 * cuts the text first, so statements that run apart stay apart; a blank line separates them in the
 * result. Keyword case stays as written. Throws when the formatter cannot read a statement.
 */
export function formatSql(text: string, dialect: SqlEngine): string {
  try {
    return statementRanges(text, dialect, true)
      .map(([from, to]) => text.slice(from, to).trim())
      .filter(Boolean)
      .map((s) => formatDialect(s, { dialect: dialects[dialect], tabWidth: 2, keywordCase: 'preserve' }))
      .join('\n\n');
  } catch (e) {
    // The parser's message goes on with its full grammar trace; its first line names the token.
    throw new Error(String(e instanceof Error ? e.message : e).split('\n')[0]);
  }
}
