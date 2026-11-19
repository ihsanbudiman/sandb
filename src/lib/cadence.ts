// A schedule's repeat rule, computed with native Date in local time. There is no
// date library in the repo; `setDate` moves across DST changes correctly.

export type Cadence =
  | { kind: 'minutes'; every: number } // 1..59
  | { kind: 'hours'; every: number } // 1..24
  | { kind: 'daily'; at: string } // 'HH:MM', 24 h, local time
  | { kind: 'weekly'; days: number[]; at: string }; // days: 0=Sun..6=Sat (Date#getDay), 1..7 entries, unique

export const DEFAULT_CADENCE: Cadence = { kind: 'daily', at: '09:00' };

const TIME = /^([01]\d|2[0-3]):[0-5]\d$/;
const DAY_NAMES = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const DAY_MS = 86_400_000;

/** A validation message for the dialog, or null when the cadence is usable. */
export function cadenceError(c: Cadence): string | null {
  switch (c.kind) {
    case 'minutes':
      return Number.isInteger(c.every) && c.every >= 1 && c.every <= 59 ? null : 'Enter a whole number from 1 to 59.';
    case 'hours':
      return Number.isInteger(c.every) && c.every >= 1 && c.every <= 24 ? null : 'Enter a whole number from 1 to 24.';
    case 'daily':
      return TIME.test(c.at) ? null : 'Enter a time as HH:MM.';
    case 'weekly':
      if (!c.days.some((d) => d >= 0 && d <= 6)) return 'Pick at least one day.';
      return TIME.test(c.at) ? null : 'Enter a time as HH:MM.';
  }
}

/** The first run time strictly after `from` (both unix ms). */
export function nextRun(c: Cadence, from: number): number {
  switch (c.kind) {
    case 'minutes':
      return from + c.every * 60_000;
    case 'hours':
      return from + c.every * 3_600_000;
    case 'daily': {
      const [h, m] = c.at.split(':').map(Number);
      const d = new Date(from);
      d.setHours(h, m, 0, 0);
      if (d.getTime() <= from) {
        // Move the day first, then set the time again: on a DST jump `at` still means local `at`.
        d.setDate(d.getDate() + 1);
        d.setHours(h, m, 0, 0);
      }
      return d.getTime();
    }
    case 'weekly': {
      const [h, m] = c.at.split(':').map(Number);
      for (let i = 0; i <= 7; i++) {
        const d = new Date(from);
        d.setDate(d.getDate() + i);
        d.setHours(h, m, 0, 0);
        if (c.days.includes(d.getDay()) && d.getTime() > from) return d.getTime();
      }
      return from + DAY_MS; // unreachable while `days` passes cadenceError
    }
  }
}

/** `n` run times in a row after `from`, for the dialog preview. */
export function nextRuns(c: Cadence, from: number, n: number): number[] {
  const out: number[] = [];
  let t = from;
  for (let i = 0; i < n; i++) {
    t = nextRun(c, t);
    out.push(t);
  }
  return out;
}

export function describeCadence(c: Cadence): string {
  switch (c.kind) {
    case 'minutes':
      return c.every === 1 ? 'Every minute' : `Every ${c.every} minutes`;
    case 'hours':
      return c.every === 1 ? 'Every hour' : `Every ${c.every} hours`;
    case 'daily':
      return `Every day at ${c.at}`;
    case 'weekly': {
      const set = new Set(c.days);
      if (set.size === 7) return `Every day at ${c.at}`;
      if (set.size === 5 && [1, 2, 3, 4, 5].every((d) => set.has(d))) return `Weekdays at ${c.at}`;
      if (set.size === 2 && set.has(0) && set.has(6)) return `Weekends at ${c.at}`;
      // Any other set: day short names in Mon→Sun order.
      const names = [1, 2, 3, 4, 5, 6, 0].filter((d) => set.has(d)).map((d) => DAY_NAMES[d]);
      return `${names.join(', ')} at ${c.at}`;
    }
  }
}

/** "Today 14:30", "Tomorrow 09:00", "Mon 09:00" within the next 6 days, else a short date. */
export function formatWhen(t: number, now = Date.now()): string {
  const d = new Date(t);
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false });
  const days = Math.round((startOfDay(d) - startOfDay(new Date(now))) / DAY_MS);
  if (days === 0) return `Today ${time}`;
  if (days === 1) return `Tomorrow ${time}`;
  if (days > 1 && days <= 6) return `${d.toLocaleDateString([], { weekday: 'short' })} ${time}`;
  return `${d.toLocaleDateString([], { month: 'short', day: 'numeric' })} ${time}`;
}

// Midnight in local time; comparing these makes the day difference DST-safe.
function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}
