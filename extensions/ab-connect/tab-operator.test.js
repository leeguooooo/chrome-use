import test from 'node:test';
import assert from 'node:assert/strict';
import { operatorSummary, targetIdOfTab } from './tab-operator.js';

const t = (key, value = '') => `${key}:${value}`;

test('the target of a tab comes from getTargets', async () => {
  const api = { getTargets: async () => [{ tabId: 4, id: 'A' }, { tabId: 5, id: 'B' }] };
  assert.equal(await targetIdOfTab(5, api), 'B');
  assert.equal(await targetIdOfTab(6, api), null);
});

test('a live operator is named with its project and conversation', () => {
  const s = operatorSummary({ operated: true, live: true, session: 'cu-a', origin: { agent: 'opencode', conversationId: 'ses_a', cwd: '/x/android-use' }, actions: [{ action: 'fill' }] }, t);
  assert.equal(s.title, 'panel_operator_live:OpenCode');
  assert.match(s.detail, /android-use · ses_a/);
  assert.equal(s.hasContext, true);
});

test('a session from an older daemon still shows its name, without context', () => {
  const s = operatorSummary({ operated: true, live: false, session: 'cu-a' }, t);
  assert.equal(s.hasContext, false);
  assert.match(s.detail, /cu-a/);
  assert.equal(operatorSummary({ operated: false }, t), null);
});
