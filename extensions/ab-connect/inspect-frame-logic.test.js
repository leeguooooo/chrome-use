import './test-i18n.js';
import assert from 'node:assert/strict';
import test from 'node:test';
import { visibilityVerdict, editorUrl, deliveryMessage, LOCKED_STATES } from './inspect-frame-logic.js';

test('send needs a fresh, fully visible, foreground panel', () => {
  assert.equal(visibilityVerdict({ isVisible: true, ratio: 1, documentVisibility: 'visible' }).ok, true);
  assert.equal(visibilityVerdict({ isVisible: false, ratio: 1, documentVisibility: 'visible' }).ok, false);
  assert.equal(visibilityVerdict({ isVisible: true, ratio: 0.7, documentVisibility: 'visible' }).ok, false);
  assert.equal(visibilityVerdict({ isVisible: true, ratio: 1, documentVisibility: 'hidden' }).ok, false);
  assert.match(visibilityVerdict({ isVisible: undefined }).message, /Use Copy/);
  assert.equal(visibilityVerdict({ timeout: true }).ok, false);
  assert.equal(visibilityVerdict(null).ok, false);
});

test('editor links stay inside the bound root', () => {
  const source = { path: 'src/pages/Order View.tsx', line: 42, column: 7 };
  assert.equal(editorUrl('vscode', '/Users/me/app', source), 'vscode://file/Users/me/app/src/pages/Order%20View.tsx:42:7');
  assert.equal(editorUrl('cursor', '/Users/me/app/', source), 'cursor://file/Users/me/app/src/pages/Order%20View.tsx:42:7');
  assert.equal(editorUrl('vscode', 'C:\\work\\app', { path: 'src/a.ts', line: 1 }), 'vscode://file/C:/work/app/src/a.ts:1:1');
  assert.match(editorUrl('webstorm', '/r', source), /^webstorm:\/\/open\?file=%2Fr%2Fsrc/);
  assert.equal(editorUrl('vscode', '/r', { path: '../etc/passwd', line: 1 }), null);
  assert.equal(editorUrl('vscode', '/r', { path: '/abs/file.ts', line: 1 }), null);
  assert.equal(editorUrl('vscode', '/Users/me/app', { path: '/Users/me/app/src/a.ts', line: 2, column: 3 }), 'vscode://file/Users/me/app/src/a.ts:2:3');
  assert.equal(editorUrl('vscode', '/Users/me/app', { path: '/Users/me/app2/src/a.ts', line: 2 }), null);
  assert.equal(editorUrl('none', '/r', source), null);
  assert.equal(editorUrl('vscode', null, source), null);
});

test('delivery messages keep stored and wake apart and lock what may have landed', () => {
  assert.equal(deliveryMessage({ state: 'sent', wake: 'accepted' }).locked, true);
  assert.match(deliveryMessage({ state: 'stored', wake: 'parked' }).text, /wake: parked/);
  assert.equal(deliveryMessage({ state: 'noSession' }).locked, false);
  assert.equal(deliveryMessage({ state: 'weird' }).locked, true);
  for (const state of ['sent', 'stored', 'unknown', 'unconfirmed']) assert.ok(LOCKED_STATES.includes(state));
});

test('matching labels', async () => {
  const { scopeLabel, evidenceLabels, revisionText, reasonText, deliveryMessage } = await import('./inspect-frame-logic.js');
  assert.equal(scopeLabel('https://test.x.com', '/'), 'test.x.com');
  assert.equal(scopeLabel('http://localhost:5173', '/admin'), 'localhost:5173/admin');
  assert.deepEqual(evidenceLabels({ evidence: ['config', 'dev-server-port'], matchedPaths: 2 }), ['project config', 'dev server', '2 source files matched']);
  assert.equal(revisionText({ relation: 'ahead', ahead: 3 }), 'local is 3 commits ahead of the deployed build');
  assert.equal(revisionText({ relation: 'diverged', ahead: 1, behind: 2 }), 'local and deployed build diverged (1 ahead, 2 behind)');
  assert.equal(revisionText(null), null);
  assert.match(reasonText('sessionEnded', { sessionLabel: 'admin-main' }), /admin-main is no longer running/);
  assert.equal(deliveryMessage({ state: 'upgradeRequired' }).locked, false);
  assert.match(deliveryMessage({ state: 'targetChanged' }).text, /Nothing was sent/);
});

test('link commands quote page URLs as one literal argument', async () => {
  const { shellQuote, linkCommand } = await import('./inspect-frame-logic.js');
  const { execFileSync } = await import('node:child_process');
  const hostile = [
    "https://test.example/$(echo PWNED)", 'https://test.example/`echo PWNED`', "https://x.example/a;echo PWNED", "https://x.example/a&b|c",
    "https://x.example/(paren)", "https://x.example/it's", "https://x.example/sp ace", "https://x.example/%24%28id%29", "https://x.example/'$(echo PWNED)'",
  ];
  for (const url of hostile) {
    // sh receives exactly the URL back: nothing was substituted or split.
    const out = execFileSync('/bin/sh', ['-c', `printf %s ${shellQuote(url, 'macos')}`]).toString();
    assert.equal(out, url);
  }
  assert.equal(shellQuote("it's", 'windows'), "'it''s'");
  assert.equal(shellQuote('a$(b)', 'windows'), "'a$(b)'");
  assert.equal(shellQuote('a’b', 'windows'), "'a’’b'");
  assert.equal(linkCommand('https://x.example/p', 'linux'), "chrome-use inspect link 'https://x.example/p'");
});
