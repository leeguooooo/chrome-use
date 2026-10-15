import './test-i18n.js';
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { configureAgentPanel } from './sidepanel-entry.js';

test('toolbar entry is enabled only after Chrome confirms the behavior', async () => {
  const calls = [];
  const result = await configureAgentPanel({
    sidePanel: {
      async setPanelBehavior(value) { calls.push(value); },
      async getPanelBehavior() { calls.push('readback'); return { openPanelOnActionClick: true }; },
    },
    action: { async setPopup(value) { calls.push(value); } },
  });
  assert.deepEqual(calls, [{ openPanelOnActionClick: true }, 'readback', { popup: '' }]);
  assert.equal(result.ready, true);
});

test('unsupported or unconfirmed entry preserves the connection popup', async () => {
  for (const sidePanel of [undefined, { async setPanelBehavior() {}, async getPanelBehavior() { return { openPanelOnActionClick: false }; } }]) {
    const calls = [];
    const result = await configureAgentPanel({ sidePanel, action: { async setPopup(value) { calls.push(value); } } });
    assert.equal(result.ready, false);
    assert.deepEqual(calls, [{ popup: 'popup.html' }]);
    assert.ok(result.error);
  }
});

test('packaged entry requires no webpage injection or new host permissions', () => {
  const manifest = JSON.parse(readFileSync(new URL('./manifest.json', import.meta.url), 'utf8'));
  assert.equal(manifest.side_panel.default_path, 'sidepanel.html');
  assert.ok(manifest.permissions.includes('sidePanel'));
  assert.equal(manifest.action.default_popup, 'popup.html');
  assert.equal(manifest.content_scripts, undefined);
  assert.equal(manifest.host_permissions, undefined);
  assert.match(readFileSync(new URL('./sidepanel.html', import.meta.url), 'utf8'), /src="sidepanel.js"/);
});
