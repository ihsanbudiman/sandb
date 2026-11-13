import { version } from '../../package.json';
import { api } from './api';
import { releasesSince, type Release } from './changelog';
import { tabs } from './tabs.svelte';
import { settlePending } from './tx.svelte';

export const appVersion = version;

/** The last version whose changes the user saw. A different version on start means the app was updated (or is new). */
const SEEN_KEY = 'sandb.version.seen';
export const whatsNew = $state({ releases: [] as Release[] });

export function showWhatsNew() {
  if (import.meta.env.DEV) return;
  const seen = localStorage.getItem(SEEN_KEY);
  if (seen === appVersion) return;
  whatsNew.releases = releasesSince(seen, appVersion);
  if (!whatsNew.releases.length) localStorage.setItem(SEEN_KEY, appVersion);
}

export function closeWhatsNew() {
  whatsNew.releases = [];
  localStorage.setItem(SEEN_KEY, appVersion);
}

type Status = 'idle' | 'checking' | 'latest' | 'available' | 'installing' | 'restart' | 'error';
export const updates = $state({ status: 'idle' as Status, next: '', notes: '', error: '' });

// The updater plugin only exists inside the desktop app.
export const updatesEnabled = api.capabilities.updates && !import.meta.env.DEV;
let found: import('@tauri-apps/plugin-updater').Update | null = null;

/** `quiet`: the startup check; a failure (offline, no release yet) shows nothing. */
export async function checkForUpdates(quiet = false) {
  if (!updatesEnabled || ['checking', 'installing', 'restart'].includes(updates.status)) return;
  updates.status = 'checking';
  try {
    const { check } = await import('@tauri-apps/plugin-updater');
    found = await check();
    if (found) Object.assign(updates, { status: 'available', next: found.version, notes: found.body ?? '' });
    else updates.status = 'latest';
  } catch (e) {
    Object.assign(updates, quiet ? { status: 'idle' } : { status: 'error', error: String(e) });
  }
}

export async function installUpdate() {
  if (!found || !(await settlePending(tabs.list, 'Installing the update', true))) return;
  updates.status = 'installing';
  try {
    await found.downloadAndInstall();
  } catch (e) {
    Object.assign(updates, { status: 'error', error: String(e) });
    return;
  }
  updates.status = 'restart';
  await restartApp();
}

export async function restartApp() {
  // A transaction can start while the update downloads, so ask again right before the restart.
  if (!(await settlePending(tabs.list, 'Restarting to update', true))) return;
  try {
    const { relaunch } = await import('@tauri-apps/plugin-process');
    await relaunch();
  } catch (e) {
    updates.error = String(e);
  }
}
