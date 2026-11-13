/** mongosh text of one value: the literal the shell reads back as the same BSON value. */

import type { TypedValue } from '../api';

export const INT64_MIN = -9223372036854775808n;
export const INT64_MAX = 9223372036854775807n;

/** A numeric string the shell reads as a number (an exponent included). */
export const MONGO_NUMERIC = /^-?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i;

/**
 * Exact decimal digits of a numeric string when its value is a whole number (`1e18` gives
 * `1000000000000000000`, `2.0` gives `2`); null when it has a fraction.
 */
function integerDigits(v: string): string | null {
  const m = /^(-?)(\d*)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/.exec(v);
  if (!m) return null;
  const [, sign, whole, fraction = '', exponent = '0'] = m;
  const digits = whole + fraction;
  if (!digits) return null;
  // The decimal point of `whole.fraction` sits after `whole`; the exponent moves it.
  const point = whole.length + Number(exponent);
  // int64 has at most 19 digits: a longer integer is out of range, and there is no point in building it.
  if (point > 20) return null;
  if (point <= 0) return /^0+$/.test(digits) ? '0' : null;
  if (point < digits.length && !/^0+$/.test(digits.slice(point))) return null;
  const all = point >= digits.length ? digits + '0'.repeat(point - digits.length) : digits.slice(0, point);
  const trimmed = all.replace(/^0+(?=\d)/, '');
  return sign ? `-${trimmed}` : trimmed;
}

/**
 * `value` as a mongosh literal of `type`. Text that cannot be the type is quoted, so it can never
 * end the string or the statement; `shell` is text the backend already formatted.
 */
export function mongoValueText(type: string, value: string): string {
  if (type === 'shell') return value;
  if (type === 'string') return JSON.stringify(value);
  const v = value.trim();
  switch (type) {
    case 'null':
    case 'missing':
      return 'null';
    case 'int64': {
      const digits = MONGO_NUMERIC.test(v) ? integerDigits(v) : null;
      if (digits !== null) {
        const n = BigInt(digits);
        if (n < INT64_MIN || n > INT64_MAX) throw new Error(`${v} does not fit in an int64 (${INT64_MIN} to ${INT64_MAX})`);
        return `NumberLong("${digits}")`;
      }
      return JSON.stringify(v);
    }
    case 'decimal128':
      return MONGO_NUMERIC.test(v) || /^(?:NaN|-?Infinity)$/.test(v) ? `NumberDecimal("${v}")` : JSON.stringify(v);
    case 'objectId':
      return /^[0-9a-f]{24}$/i.test(v) ? `ObjectId("${v}")` : JSON.stringify(v);
    case 'uuid':
      return `UUID(${JSON.stringify(v)})`;
    case 'date':
      return `ISODate(${JSON.stringify(v)})`;
    case 'bool':
      return /^(true|false)$/i.test(v) ? v.toLowerCase() : JSON.stringify(v);
    // The shell reads `42` as an int32; a whole double keeps its `.0`.
    case 'double':
      return MONGO_NUMERIC.test(v) ? (/^-?\d+$/.test(v) ? `${v}.0` : v) : /^(?:NaN|-?Infinity)$/.test(v) ? v : JSON.stringify(v);
    case 'int32':
    case 'number':
      return MONGO_NUMERIC.test(v) ? v : JSON.stringify(v);
    default:
      return JSON.stringify(v);
  }
}

/** The literal of one typed value. */
export const mongoLiteral = (v: TypedValue): string => mongoValueText(v.type, v.value ?? '');
