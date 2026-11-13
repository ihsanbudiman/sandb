import { getCurrentWindow, Effect } from '@tauri-apps/api/window';
import { api } from './api';
import { readStored, writeStored } from './persist.svelte';
import { ui } from './state.svelte';

export type Theme = 'light' | 'dark' | 'system';
export type Backdrop = 'none' | 'image' | 'desktop';

/** Only the desktop app has a transparent window. In a browser tab there is nothing behind the page. */
export const canSeeThrough = api.capabilities.windowEffects;

const KEY = 'sandb.appearance';
// The image is large, so it has its own key: a slider move does not write it again.
const IMAGE_KEY = 'sandb.appearance.image';
// 100% on the blur sliders.
const IMAGE_BLUR_MAX_PX = 40;
const PANEL_BLUR_MAX_PX = 48;
// Longest side of the stored image. Bigger images fill the localStorage quota that chats and saved queries share.
const IMAGE_MAX_PX = 1920;

const defaults = {
  backdrop: 'none' as Backdrop,
  /** Canvas color over the backdrop, in %. 100 hides the backdrop. */
  tint: 65,
  /** Opacity of the main card and the assistant panel, in %. */
  panels: 85,
  /** Blur of the image, in % of IMAGE_BLUR_MAX_PX. */
  imageBlur: 30,
  /** Blur behind the glass panels, in % of PANEL_BLUR_MAX_PX. Applies while panel opacity is below 100%. */
  panelBlur: 50,
  /** See-through only: the system blurs the desktop behind the window. */
  vibrancy: true,
};

export const look = $state({ ...defaults, image: '' });

export const backdropOn = () => (look.backdrop === 'image' ? !!look.image : look.backdrop === 'desktop' && canSeeThrough);

function save() {
  const { backdrop, tint, panels, imageBlur, panelBlur, vibrancy } = look;
  writeStored(KEY, { theme: ui.theme, backdrop, tint, panels, imageBlur, panelBlur, vibrancy }, { delay: 200, onError: (e) => console.warn('Appearance was not saved.', e) });
}

// The window starts with no native effect, so the first sync only calls the API when the effect is wanted.
let nativeBlur = false;

async function syncNativeBlur(want: boolean) {
  if (!canSeeThrough || want === nativeBlur) return;
  nativeBlur = want;
  try {
    const w = getCurrentWindow();
    if (want) await w.setEffects({ effects: [Effect.UnderWindowBackground] });
    else await w.clearEffects();
  } catch (e) {
    nativeBlur = !want;
    console.warn('The window blur was not changed.', e);
  }
}

function apply() {
  const root = document.documentElement;
  const on = backdropOn();
  root.classList.toggle('backdrop', on);
  // Glass on the panels costs a blur pass per frame; skip it while they are opaque.
  root.classList.toggle('glass', on && look.panels < 100);
  root.style.setProperty('--backdrop-tint', String(look.tint / 100));
  root.style.setProperty('--backdrop-blur', `${(look.imageBlur / 100) * IMAGE_BLUR_MAX_PX}px`);
  root.style.setProperty('--panel-blur', `${(look.panelBlur / 100) * PANEL_BLUR_MAX_PX}px`);
  root.style.setProperty('--panel-alpha', String(on ? look.panels / 100 : 1));
  syncNativeBlur(on && look.backdrop === 'desktop' && look.vibrancy);
}

/** Applies and stores the current `look`. Call after each edit. */
export function lookChanged() {
  apply();
  save();
}

export function setTheme(t: Theme) {
  ui.theme = t;
  ui.dark = t === 'system' ? matchMedia('(prefers-color-scheme: dark)').matches : t === 'dark';
  document.documentElement.classList.toggle('dark', ui.dark);
  save();
}

export function toggleTheme() {
  setTheme(ui.dark ? 'light' : 'dark');
}

/** Reads the stored appearance and applies it. Call once, before the app mounts. */
export function loadAppearance() {
  const stored = readStored<Partial<typeof defaults & { theme: Theme }>>(KEY, {}, { migrate: (data) => (data && typeof data === 'object' ? (data as Partial<typeof defaults & { theme: Theme }>) : {}) });
  for (const k of Object.keys(defaults) as (keyof typeof defaults)[]) {
    if (typeof stored[k] === typeof defaults[k]) (look as Record<string, unknown>)[k] = stored[k];
  }
  if (look.backdrop === 'desktop' && !canSeeThrough) look.backdrop = 'none';
  look.image = localStorage.getItem(IMAGE_KEY) ?? '';
  const theme = stored.theme === 'dark' || stored.theme === 'system' ? stored.theme : 'light';
  ui.theme = theme;
  ui.dark = theme === 'system' ? matchMedia('(prefers-color-scheme: dark)').matches : theme === 'dark';
  document.documentElement.classList.toggle('dark', ui.dark);
  apply();
}

/** Scales the image down, stores it as a JPEG data URL and shows it. Fails when the file is not an image or storage is full. */
export async function setBackgroundImage(file: File) {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new Error('This file is not an image that sandb can read.');
  }
  const scale = Math.min(1, IMAGE_MAX_PX / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement('canvas');
  canvas.width = Math.round(bitmap.width * scale);
  canvas.height = Math.round(bitmap.height * scale);
  canvas.getContext('2d')!.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close();
  const url = canvas.toDataURL('image/jpeg', 0.85);
  try {
    localStorage.setItem(IMAGE_KEY, url);
  } catch {
    throw new Error('There is not enough storage for this image. Try a smaller one.');
  }
  look.image = url;
  look.backdrop = 'image';
  lookChanged();
}

export function removeBackgroundImage() {
  localStorage.removeItem(IMAGE_KEY);
  look.image = '';
  look.backdrop = 'none';
  lookChanged();
}

/** Back to the plain canvas with default sliders. Keeps the theme and the stored image. */
export function resetBackdrop() {
  Object.assign(look, defaults);
  lookChanged();
}
