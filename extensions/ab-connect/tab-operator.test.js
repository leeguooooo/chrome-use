import test from 'node:test';
import assert from 'node:assert/strict';
import { handoffSummary, operatorSummary, targetIdOfTab } from './tab-operator.js';

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

test('a handed-off tab shows the ask, the condition and whether done was sent', () => {
  const s = handoffSummary({ operated: true, live: true, handoff: { note: 'Sign in', until: { kind: 'url', value: '**/home' }, done: false } }, t);
  assert.equal(s.title, 'panel_handoff_title:');
  assert.equal(s.note, 'Sign in');
  assert.equal(s.condition, 'panel_handoff_until_url:**/home');
  assert.equal(s.done, false);
  const bare = handoffSummary({ operated: true, handoff: { since: null, until: null, note: null, done: true } }, t);
  assert.equal(bare.condition, '');
  assert.equal(bare.note, '');
  assert.equal(bare.done, true);
  assert.equal(handoffSummary({ operated: true, handoff: null }, t), null);
  assert.equal(handoffSummary({ operated: false }, t), null);
});
