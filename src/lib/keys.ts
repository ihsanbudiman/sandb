/**
 * One keyboard table: the CodeMirror keymap and every hint that advertises a shortcut read these
 * entries, so a binding and its label cannot drift apart. `combo` is a CodeMirror key string; `hint`
 * is the text the UI shows.
 */
export const KEYS = {
  run: { combo: 'Mod-Enter', hint: '⌘↵' },
  runNew: { combo: 'Mod-Shift-Enter', hint: '⇧⌘↵' },
  runAll: { combo: 'Alt-x', hint: '⌥X' },
  format: { combo: 'Mod-Shift-f', hint: '⇧⌘F' },
  save: { combo: 'Mod-s', hint: '⌘S' },
  saveAs: { combo: 'Mod-Shift-s', hint: '⇧⌘S' },
  palette: { combo: 'Mod-k', hint: '⌘K' },
  toggleSteps: { combo: 'Mod-Shift-h', hint: '⇧⌘H' },
  foldStatement: { combo: 'Mod-Alt-[', hint: '' },
  foldAll: { combo: 'Ctrl-Alt-[', hint: '' },
  unfoldAll: { combo: 'Ctrl-Alt-]', hint: '' },
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
