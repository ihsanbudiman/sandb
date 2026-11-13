import changelog from '../../CHANGELOG.md?raw';

export type Release = { version: string; date: string; notes: string };

/** The released sections of CHANGELOG.md (`## [x.y.z] - date`), newest first. `[Unreleased]` is left out. */
export function parseChangelog(md: string): Release[] {
  const out: Release[] = [];
  for (const part of md.split(/^## /m).slice(1)) {
    const head = /^\[(\d+\.\d+\.\d+)\](?:\s*-\s*(\S+))?/.exec(part);
    if (!head) continue;
    out.push({ version: head[1], date: head[2] ?? '', notes: part.slice(part.indexOf('\n') + 1).trim() });
  }
  return out;
}

export function compareVersions(a: string, b: string) {
  const pa = a.split('.').map(Number);
  const pb = b.split('.').map(Number);
  for (let i = 0; i < 3; i++) if (pa[i] !== pb[i]) return (pa[i] ?? 0) - (pb[i] ?? 0);
  return 0;
}

/** Releases after `seen` up to `current`. Without `seen` (first start), only `current`. */
export function releasesSince(seen: string | null, current: string, md = changelog): Release[] {
  return parseChangelog(md).filter((r) => compareVersions(r.version, current) <= 0 && (seen ? compareVersions(r.version, seen) > 0 : r.version === current));
}
