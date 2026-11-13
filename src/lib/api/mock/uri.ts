import type { MongoUriParts } from '../generated/wire';

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
