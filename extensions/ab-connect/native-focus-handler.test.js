import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { prepareNativeFocusRecovery } from './native-focus.js';
import { resolveSessionTab } from './tab-liveness.js';

const source = fs.readFileSync(new URL('./background.js', import.meta.url), 'utf8');
const fragment = source.slice(source.indexOf("  if (method === 'ABExt.nativeFocusFinish')"), source.indexOf("  if (method === 'ABExt.blurFocusedField')"));
function fixture() {
  const calls = [], token = 'a'.repeat(32);
  let focused = false;
  const state = {
    loadOwnedTabs: async () => {}, childSessionToTab: new Map(),
    sessionToTab: new Map([['cb-tab-7', 7]]), ownedTabs: new Set([7]),
    resolveSessionTab, tabForTarget: id => id === 'live-target' ? 7 : null,
    guardTab: () => {}, prepareNativeFocusRecovery, withRelayTimeout: promise => promise,
    nativeFocusContexts: new Map(), setTimeout: () => {},
    nativeFocusBridge: { revoke: value => { assert.equal(value, token); calls.push('revoke'); return true; }, request: async operation => {
      calls.push(operation);
      if (operation === 'prepare') return { token, leaseMs: 120000 };
      if (operation === 'inspect') return { token, verified: true };
      if (operation === 'escape') return { input: { keyDownAdmitted: 1, keyUpAdmitted: 1, delivery: 'unverified', replayed: false } };
      return { expired: false, restore: { attempted: true, restored: true } };
    } },
    chrome: { debugger: { getTargets: async () => [{ id: 'live-target', tabId: 7, type: 'page' }] }, tabs: {
      get: async () => ({ id: 7, windowId: 9, url: 'https://example.invalid/login', active: true }),
      query: async () => [{ id: 7 }],
    }, windows: {
      get: async () => ({ id: 9, type: 'normal', state: 'normal', focused }),
      update: async () => { calls.push('focus'); focused = true; },
    } },
  };
  const handler = vm.runInNewContext(`(async (method, params, sessionId, agentSession) => {${fragment}})`, state);
  return { state, calls, handler, params: { allowFocusRecovery: true, targetId: 'live-target', sessionId: 'cb-tab-7', chromeTabId: 7, url: 'https://example.invalid/login' } };
}

test('real native handlers reserve authority before activation and close exactly that context', async () => {
  const f = fixture();
  const prepared = await f.handler('ABExt.nativeFocusPrepare', f.params);
  assert.equal(prepared.prepared, true);
  assert.deepEqual(f.calls, ['prepare']);
  const begun = await f.handler('ABExt.nativeFocusBegin', { token: prepared.token });
  assert.equal(begun.started, true);
  assert.deepEqual(f.calls, ['prepare', 'focus', 'inspect', 'escape']);
  await f.handler('ABExt.nativeFocusFinish', { token: prepared.token });
  assert.equal(f.state.nativeFocusContexts.size, 0);
  assert.deepEqual(f.calls.slice(-2), ['revoke', 'finish']);
});

test('a missing attachment cache still requires an exact browser-registry target', async () => {
  const f = fixture();
  f.state.tabForTarget = () => null;
  const prepared = await f.handler('ABExt.nativeFocusPrepare', f.params);
  assert.equal(prepared.prepared, true);
  assert.deepEqual(f.calls, ['prepare']);
  const wrong = fixture();
  wrong.state.tabForTarget = () => null;
  wrong.state.chrome.debugger.getTargets = async () => [{ id: 'live-target', tabId: 8, type: 'page' }];
  await assert.rejects(wrong.handler('ABExt.nativeFocusPrepare', wrong.params));
  assert.deepEqual(wrong.calls, []);
});

test('cached unknown targets, mismatched tab ids and child sessions cannot prepare native input', async () => {
  for (const alter of [
    f => { f.params.targetId = 'stale-target'; },
    f => { f.params.chromeTabId = 8; },
    f => { f.state.childSessionToTab.set('cb-tab-7', 7); },
  ]) {
    const f = fixture(); alter(f);
    await assert.rejects(f.handler('ABExt.nativeFocusPrepare', f.params));
    assert.deepEqual(f.calls, []);
  }
});
