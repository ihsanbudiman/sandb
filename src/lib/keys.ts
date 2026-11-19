/**
 * One keyboard table: the CodeMirror keymap and every hint that advertises a shortcut read these
 * entries, so a binding and its label cannot drift apart. `combo` is a CodeMirror key string; `hint`
 * is the text the UI shows.
 */
export function shortcutHint(combo: string, platform = typeof navigator === 'undefined' ? '' : navigator.platform): string {
  const mac = /Mac|iPhone|iPad|iPod/i.test(platform);
  const parts = combo.split('-');
  const key = ['Mod', 'Ctrl', 'Alt', 'Shift'].includes(parts.at(-1)!) ? '' : parts.pop()!;
  if (mac) {
    return `${parts.includes('Ctrl') ? '⌃' : ''}${parts.includes('Alt') ? '⌥' : ''}${parts.includes('Shift') ? '⇧' : ''}${parts.includes('Mod') ? '⌘' : ''}${key === 'Enter' ? '↵' : key.toUpperCase()}`;
  }
  return [
    ...(parts.includes('Mod') || parts.includes('Ctrl') ? ['Ctrl'] : []),
    ...(parts.includes('Alt') ? ['Alt'] : []),
    ...(parts.includes('Shift') ? ['Shift'] : []),
    key === 'Enter' ? 'Enter' : key.toUpperCase(),
  ].filter(Boolean).join('+');
}

function shortcut<const T extends string>(combo: T) {
  return { combo, hint: shortcutHint(combo) };
}

export const KEYS = {
  run: shortcut('Mod-Enter'),
  runNew: shortcut('Mod-Shift-Enter'),
  runAll: shortcut('Alt-x'),
  format: shortcut('Mod-Shift-f'),
  save: shortcut('Mod-s'),
  saveAs: shortcut('Mod-Shift-s'),
  palette: shortcut('Mod-k'),
  toggleSteps: shortcut('Mod-Shift-h'),
  foldStatement: shortcut('Mod-Alt-['),
  foldAll: shortcut('Ctrl-Alt-['),
  unfoldAll: shortcut('Ctrl-Alt-]'),
} as const;

const MODIFIERS = ['Mod', 'Shift', 'Alt', 'Ctrl'] as const;

/**
 * Whether `e` is the key `combo` names. `Mod` is ⌘ or Ctrl. The listed modifiers must be down; extra
 * ones are allowed, so `Mod-s` also matches ⇧⌘S (the ⌘S handler passes the shift on).
 */
export function isKey(e: KeyboardEvent, combo: string): boolean {
  const parts = combo.split('-');
  const key = parts.pop()?.toLowerCase() ?? '';
  const mods = parts as (typeof MODIFIERS)[number][];
  if (mods.includes('Mod') !== (e.metaKey || e.ctrlKey)) return false;
  if (mods.includes('Shift') && !e.shiftKey) return false;
  if (mods.includes('Alt') && !e.altKey) return false;
  if (mods.includes('Ctrl') && !e.ctrlKey) return false;
  return e.key.toLowerCase() === key;
}
