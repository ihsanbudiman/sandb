import { api } from './api';

/** One OS notification; asks for the permission the first time. Never throws: a refused or failed notification only logs. */
export async function osNotify(title: string, body: string): Promise<void> {
  if (!api.capabilities.notifications) return;
  try {
    // The plugin only exists inside the desktop app, so load it lazily like the updater does.
    const { isPermissionGranted, requestPermission, sendNotification } = await import('@tauri-apps/plugin-notification');
    let ok = await isPermissionGranted();
    if (!ok) ok = (await requestPermission()) === 'granted';
    if (ok) sendNotification({ title, body });
  } catch (e) {
    console.warn('The notification was not shown.', e);
  }
}
