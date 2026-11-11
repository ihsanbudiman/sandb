import type { SslMode } from './api';

/** A MongoDB connection string split like the backend's `parse_uri` (core/mongo/conn.rs); null when it is not one. */
export interface MongoUri {
  /** The string without its password, as the backend saves it. */
  redacted: string;
  hasPassword: boolean;
  /** Comma-joined host list, or the SRV host. */
  host: string;
  /** Port of the first host; 27017 when it has none. */
  port: number;
  user: string;
  database: string;
  authSource: string;
  ssl: SslMode;
}

export const isMongoUri = (uri: string) => /^mongodb(\+srv)?:\/\//i.test(uri.trim());

export function parseMongoUri(uri: string): MongoUri | null {
  const s = uri.trim();
  if (!isMongoUri(s)) return null;
  const cut = s.indexOf('://');
  const scheme = s.slice(0, cut);
  const rest = s.slice(cut + 3);
  const q = rest.indexOf('?');
  const pre = q < 0 ? rest : rest.slice(0, q);
  const options = q < 0 ? '' : rest.slice(q + 1);
  // The user info ends at the last `@` before the options, so a password with an unencoded `@` still goes.
  const at = pre.lastIndexOf('@');
  const userInfo = at < 0 ? '' : pre.slice(0, at);
  const target = pre.slice(at + 1);
  const colon = userInfo.indexOf(':');
  const rawUser = colon < 0 ? userInfo : userInfo.slice(0, colon);
  const slash = target.indexOf('/');
  const hosts = (slash < 0 ? target : target.slice(0, slash)).split(',').filter(Boolean);
  const split = hosts.map((h) => /^(\[[^\]]*\]|[^:]*)(?::(\d+))?$/.exec(h) ?? [h, h, undefined]);
  const params = new URLSearchParams(options);
  const tls = params.get('tls') ?? params.get('ssl');
  const srv = scheme.toLowerCase() === 'mongodb+srv';
  const decode = (v: string) => {
    try {
      return decodeURIComponent(v);
    } catch {
      return v;
    }
  };
  return {
    redacted: at < 0 ? s : `${scheme}://${rawUser}@${target}${q < 0 ? '' : '?' + options}`,
    hasPassword: colon >= 0 && colon < userInfo.length - 1,
    host: split.map((m) => m[1]).join(','),
    port: Number(split[0]?.[2]) || 27017,
    user: decode(rawUser),
    database: decode(slash < 0 ? '' : target.slice(slash + 1)),
    authSource: params.get('authSource') ?? '',
    ssl: (tls === null ? srv : tls.toLowerCase() === 'true') ? 'require' : 'disable',
  };
}
