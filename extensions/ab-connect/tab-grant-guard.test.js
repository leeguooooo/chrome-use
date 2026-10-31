// The background's guard on lent and taken-back tabs, run on the real
// background.js sections: every step of a batch and the chrome.tabs
// navigation fallback check the grant again, and a lent tab answers only the
// relay session of its conversation.
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { isAttachmentCleanup } from './tab-command.js';
import { createTabGrants, TAKEN_BACK, LENT_ELSEWHERE } from './tab-grant.js';
import { canUseBrowserNavigationFallback, navigateTabWithBrowserFallback } from './tab-navigation.js';

const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
const section = (start, end) => { const from = source.indexOf(start); const to = source.indexOf(end, from); assert.ok(from >= 0 && to > from, start); return source.slice(from, to); };
function memory() { const data = {}; return { get: async key => (key in data ? { [key]: data[key] } : {}), set: async value => Object.assign(data, JSON.parse(JSON.stringify(value))) }; }
const tab = id => ({ id, url: `https://shop.example/${id}`, title: `Tab ${id}` });

function worker(tabGrants, { onSend = async () => ({}) } = {}) {
  const sent = [], updated = [];
  const context = {
    tabGrants, sent, updated, console, isAttachmentCleanup,
    tabs: new Map([[7, { attached: true }], [8, { attached: true }]]),
    conversationWindows: { observe: async () => {} },
    canUseBrowserNavigationFallback, navigateTabWithBrowserFallback,
    withRelayTimeout: p => p,
    maybeDriveCursor() {},
    sendCdpToTab: async (tabId, method) => { sent.push(method); return onSend(method); },
    chrome: { tabs: { update: async (tabId, props) => { updated.push(props.url); return {}; } } },
    resolveSessionTab: sessionId => Number(/cb-tab-(\d+)/.exec(sessionId)?.[1] ?? NaN),
    tabIdFromSession: sessionId => Number(/cb-tab-(\d+)/.exec(sessionId)?.[1] ?? NaN),
    sessionToTab: new Map(), childSessionToTab: new Map(),
    tabForTarget: () => null, anyConnectedTab: () => null,
  };
  vm.createContext(context);
  vm.runInContext(section('function guardTab(', '// Read-only reports'), context);
  vm.runInContext(section('async function dispatchToTab(', '// ---- attach / detach'), context);
  // The batch branch, as a function of the forwarded command.
  vm.runInContext(`globalThis.batch = async function (sessionId, params, agentSession) { const method = 'ABExt.batchCommands';\n${section("if (method === 'ABExt.batchCommands') {", '// Browser-level Target methods')}}`, context);
  return context;
}

test('a lent tab answers only its conversation\'s session, after a take-back and a new grant too', async () => {
  const grants = createTabGrants({ storage: memory() });
  await grants.grant(tab(7), { conversationId: 'A' });
  await grants.revoke(7);
  await grants.grant(tab(7), { conversationId: 'B' });
  const w = worker(grants);
  await assert.rejects(w.batch('cb-tab-7', { commands: [{ method: 'Runtime.evaluate' }] }, 'panel-A'), error => error.message === LENT_ELSEWHERE);
  await assert.rejects(w.batch('cb-tab-7', { commands: [{ method: 'Runtime.evaluate' }] }, ''), error => error.message === LENT_ELSEWHERE);
  await assert.rejects(w.batch('cb-tab-7', { commands: [{ method: 'Runtime.evaluate' }] }, 'default'), error => error.message === LENT_ELSEWHERE);
  assert.deepEqual(w.sent, []);
  const ok = await w.batch('cb-tab-7', { commands: [{ method: 'Runtime.evaluate' }] }, 'panel-B');
  assert.equal(ok.results[0].success, true);
  assert.deepEqual(w.sent, ['Runtime.evaluate']);
  // A tab nobody lent is anyone's, as before.
  const free = await w.batch('cb-tab-8', { commands: [{ method: 'Runtime.evaluate' }] }, 'default');
  assert.equal(free.results[0].success, true);
});

test('a take-back during a batch stops its later steps, the chrome.tabs navigation included', async () => {
  const grants = createTabGrants({ storage: memory() });
  await grants.grant(tab(7), { conversationId: 'B' });
  const w = worker(grants, { onSend: async method => { if (method === 'Runtime.evaluate') await grants.revoke(7); return {}; } });
  const result = await w.batch('cb-tab-7', { stopOnError: false, commands: [{ method: 'Runtime.evaluate' }, { method: 'Page.navigate', params: { url: 'https://elsewhere.example/' } }, { method: 'Input.dispatchKeyEvent' }] }, 'panel-B');
  assert.deepEqual(w.sent, ['Runtime.evaluate']);
  assert.deepEqual(w.updated, [], 'chrome.tabs.update never ran');
  assert.equal(result.results[1].success, false);
  assert.equal(result.results[1].error, TAKEN_BACK);
  assert.equal(result.results[2].error, TAKEN_BACK);
});


test('a batch with background cleanup refuses sessionless selection before any step', async () => {
  const w = worker(createTabGrants({ storage: memory() }));
  w.resolveSessionTab = () => null;
  w.anyConnectedTab = () => 7;
  await assert.rejects(w.batch(undefined, { commands: [
    { method: 'Runtime.evaluate' },
    { method: 'Emulation.setDefaultBackgroundColorOverride', params: {} },
  ] }, 'default'), /requires an explicit attached session/);
  assert.deepEqual(w.sent, []);
});

for (const state of ['stale', 'detached']) {
  test(`a batch with background cleanup refuses ${state} session before any step`, async () => {
    const w = worker(createTabGrants({ storage: memory() }));
    if (state === 'stale') w.resolveSessionTab = () => null;
    else w.tabs.get(7).attached = false;
    w.anyConnectedTab = () => 8;
    await assert.rejects(w.batch('cb-tab-7', { commands: [
      { method: 'Runtime.evaluate' },
      { method: 'Emulation.setDefaultBackgroundColorOverride', params: {} },
    ] }, 'default'), /existing attachment|no attached tab/);
    assert.deepEqual(w.sent, []);
  });
}

test('a bound batch clears the background once but refuses cleanup after attachment loss', async () => {
  const w = worker(createTabGrants({ storage: memory() }));
  const reset = { method: 'Emulation.setDefaultBackgroundColorOverride', params: {} };
  const ok = await w.batch('cb-tab-7', { commands: [reset] }, 'default');
  assert.equal(ok.results[0].success, true);
  assert.deepEqual(w.sent, [reset.method]);
  w.sendCdpToTab = async (_id, method) => { w.sent.push(method); w.tabs.get(7).attached = false; return {}; };
  const lost = await w.batch('cb-tab-7', { commands: [{ method: 'Runtime.evaluate' }, reset] }, 'default');
  assert.equal(lost.results[1].success, false);
  assert.match(lost.results[1].error, /existing attachment/);
  assert.deepEqual(w.sent, [reset.method, 'Runtime.evaluate']);
});
