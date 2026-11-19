import type { MongoUriParts, RedisUriParts } from '../generated/wire';
import { ApiError } from '../types';

/** The string with its password masked, or null when it is not a MongoDB connection string. */
export function redactMongoUri(uri: string): string | null {
  const s = uri.trim();
  if (!/^mongodb(\+srv)?:\/\//i.test(s)) return null;
  const scheme = s.indexOf('://') + 3;
  const q = s.indexOf('?');
  const at = s.lastIndexOf('@', q < 0 ? s.length : q);
  const colon = s.indexOf(':', scheme);
  return at < 0 || colon < 0 || colon > at ? s : `${s.slice(0, colon)}${s.slice(at)}`;
}

/**
 * The demo has no driver: only the redaction is known, and the fields stay empty rather than being
 * guessed. `null` for every string the scheme check rejects.
 */
export function parseMongoUri(uri: string): MongoUriParts | null {
  const redacted = redactMongoUri(uri);
  if (redacted === null) return null;
  return { redacted, host: '', port: 0, user: '', database: '', authSource: '', ssl: /^mongodb\+srv:/i.test(redacted) ? 'require' : 'disable' };
}

const invalid = (message: string) => new ApiError({ code: 'invalid_input', message });
const databaseError = 'Database must be a number, for example 0';

export function redisDatabase(value: string): string {
  if (!value) return '0';
  if (!/^\d+$/.test(value) || Number(value) > 0xffff_ffff) throw invalid(databaseError);
  return String(Number(value));
}

function redisHostValid(host: string): boolean {
  if (host.startsWith('[') && host.endsWith(']')) {
    try {
      return new URL(`redis://${host}`).hostname.startsWith('[');
    } catch {
      return false;
    }
  }
  return host.replace(/\.$/, '').split('.').every((label) => /^[a-z\d_-]+$/i.test(label) && /[a-z\d]/i.test(label));
}

export function redisNodeValid(entry: string): boolean {
  const parts = /^(\[[^\]]+\]|[^:[\]]+)(?::(\d+))?$/.exec(entry);
  if (!parts || !redisHostValid(parts[1])) return false;
  return parts[2] === undefined || (Number(parts[2]) > 0 && Number(parts[2]) <= 65535);
}

// Redis's URL decoder leaves a percent sign without a hex pair as a literal percent sign.
const decodeUserInfo = (part: string) => decodeURIComponent(part.replace(/%(?![0-9a-f]{2})/gi, '%25'));

/** Parse the standalone URL fields, never returning its password or URL options. */
export function redisUriParts(uri: string): RedisUriParts {
  const text = uri.trim();
  const scheme = /^rediss?:\/\//i.exec(text);
  if (!scheme) throw invalid('A Redis connection string starts with redis:// or rediss://');
  const base = text.split(/[?#]/, 1)[0];
  const authority = base.slice(scheme[0].length).split('/', 1)[0];
  const address = authority.slice(authority.lastIndexOf('@') + 1);
  if (!address) throw invalid('The connection string has no host');
  if (/\s/.test(base) || address.endsWith(':')) throw invalid('Invalid connection string');
  let url: URL;
  try {
    url = new URL(base);
  } catch {
    throw invalid('Invalid connection string');
  }
  const host = url.hostname;
  if (!host) throw invalid('The connection string has no host');
  if (!redisHostValid(host) || url.port === '0') throw invalid('Invalid connection string');
  const database = redisDatabase(url.pathname.slice(1));
  let user: string;
  try {
    user = decodeUserInfo(url.username);
    decodeUserInfo(url.password);
  } catch {
    throw invalid('Invalid connection string');
  }
  url.password = '';
  return { redacted: url.href, host, port: url.port ? Number(url.port) : 6379, user, database, ssl: url.protocol === 'rediss:' ? 'require' : 'disable' };
}

export function parseRedisUri(uri: string): RedisUriParts | null {
  try {
    return redisUriParts(uri);
  } catch {
    return null;
  }
}
