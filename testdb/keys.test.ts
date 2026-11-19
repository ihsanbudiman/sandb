import assert from 'node:assert/strict';
import { test } from 'node:test';
import { shortcutHint, KEYS } from '../src/lib/keys.ts';

for (const platform of ['Linux x86_64', 'Win32']) {
  test(`shortcut hints use Ctrl on ${platform}`, () => {
    assert.equal(shortcutHint(KEYS.run.combo, platform), 'Ctrl+Enter');
    assert.equal(shortcutHint(KEYS.saveAs.combo, platform), 'Ctrl+Shift+S');
    assert.equal(shortcutHint(KEYS.runAll.combo, platform), 'Alt+X');
    assert.equal(shortcutHint('Mod', platform), 'Ctrl');
  });
}

test('macOS hints retain familiar modifier glyphs', () => {
  assert.equal(shortcutHint(KEYS.run.combo, 'MacIntel'), '⌘↵');
  assert.equal(shortcutHint(KEYS.saveAs.combo, 'MacIntel'), '⇧⌘S');
  assert.equal(shortcutHint(KEYS.runAll.combo, 'MacIntel'), '⌥X');
  assert.equal(shortcutHint(KEYS.foldAll.combo, 'MacIntel'), '⌃⌥[');
  assert.equal(shortcutHint('Mod', 'MacIntel'), '⌘');
});
