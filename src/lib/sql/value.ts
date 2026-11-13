/** SQL identifiers and literals, encoded once for the grid preview, the filters and the schema defaults. */

import type { Cell, Engine } from '../api';

/** A column whose values the SQL text may write unquoted. */
export function isNumericType(dataType: string): boolean {
  return /^(int|integer|smallint|tinyint|mediumint|bigint|int[248]|serial|bigserial|smallserial|real|double|float|float[48]|decimal|numeric|number|money)\b/i.test(dataType);
}

/** A numeric string the SQL text may take as written (an exponent included). */
export const isNumericText = (text: string) => /^-?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i.test(text);

/** Always quoted: names from the tree may be reserved words, mixed case or contain any character. */
export function quoteIdent(engine: Engine, name: string): string {
  return engine === 'mysql' ? '`' + name.replace(/`/g, '``') + '`' : '"' + name.replace(/"/g, '""') + '"';
}

/**
 * `v` as SQL text. A string is written unquoted only when the column's type says it is a number or a
 * boolean; everything else is quoted, so no value can end the literal or the statement.
 */
export function sqlLiteral(engine: Engine, v: Cell, dataType = ''): string {
  if (v === null) return 'NULL';
  if (typeof v === 'number') return String(v);
  if (typeof v === 'boolean') return engine === 'pg' ? (v ? 'TRUE' : 'FALSE') : v ? '1' : '0';
  if (typeof v === 'string') {
    const text = v.trim();
    if (isNumericType(dataType) && isNumericText(text)) return text;
    if (/bool/i.test(dataType) && /^(true|false|1|0)$/i.test(text)) {
      const yes = /^(true|1)$/i.test(text);
      return engine === 'pg' ? (yes ? 'TRUE' : 'FALSE') : yes ? '1' : '0';
    }
  }
  let s = (typeof v === 'object' ? JSON.stringify(v) : v).replace(/'/g, "''");
  if (engine === 'mysql') s = s.replace(/\\/g, '\\\\');
  return `'${s}'`;
}
