import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeFocusBridge, beginNativeFocusRecovery, prepareNativeFocusRecovery } from './native-focus.js';

const token = 'a'.repeat(32);
const params = { allowFocusRecovery: true, url: 'https://example.invalid/login' };
function fixture() {
  const calls = [];
  const state = { owned: true, focused: false, tab: { id: 1, url: params.url, active: true, windowId: 7 } };
  const deps = {
    boundTab: () => 1, isOwned: () => state.owned, guard: () => {},
    getTab: async () => ({ ...state.tab }),
    getWindow: async () => ({ id: 7, type: 'normal', state: 'normal', focused: state.focused }),
    getWindowTabs: async () => [{ ...state.tab }],
    focusWindow: async id => { calls.push(['focus', id]); state.focused = true; },
    request: async operation => {
      calls.push([operation]);
      if (operation === 'prepare') return { schemaVersion: 1, token, leaseMs: 120000 };
      if (operation === 'inspect') return { token, verified: true };
      if (operation === 'escape') return { input: { keyDownAdmitted: 1, keyUpAdmitted: 1, delivery: 'unverified', replayed: false } };
      return { schemaVersion: 1, expired: false, restore: { attempted: true, restored: true } };
    },
  };
  return { calls, state, deps };
}

test('only explicit authority and an exact owned active page can request focus', async () => {
  for (const change of [
    f => { f.state.owned = false; },
    f => { f.state.tab.url = 'https://example.invalid/other'; },
    f => { f.state.tab.active = false; },
    f => { f.state.tab.pendingUrl = 'https://example.invalid/other'; },
    f => { f.deps.boundTab = () => null; },
    f => { f.deps.guard = () => { throw new Error('user took tab back'); }; },
    f => { f.deps.getWindowTabs = async () => [{ id: 2 }]; },
  ]) {
    const f = fixture(); change(f);
    await assert.rejects(beginNativeFocusRecovery(params, f.deps));
    assert.deepEqual(f.calls, []);
  }
  const f = fixture();
  await assert.rejects(beginNativeFocusRecovery({ ...params, allowFocusRecovery: false }, f.deps));
  assert.deepEqual(f.calls, []);
});

test('one successful attempt leaves its lease open until the authorized command finishes', async () => {
  const f = fixture();
  const result = await beginNativeFocusRecovery(params, f.deps);
  assert.equal(result.started, true);
  assert.equal(result.token, token);
  assert.equal(result.input.delivery, 'unverified');
  assert.deepEqual(f.calls, [['prepare'], ['focus', 7], ['inspect'], ['escape']]);
});

test('revocation during native preparation closes the lease without focusing or sending Escape', async () => {
  const f = fixture(), original = f.deps.request;
  f.deps.request = async op => { const result = await original(op); if (op === 'prepare') f.state.owned = false; return result; };
  const result = await beginNativeFocusRecovery(params, f.deps);
  assert.equal(result.started, false);
  assert.equal(result.focusRequested, false);
  assert.deepEqual(f.calls, [['prepare'], ['finish']]);
});

test('failed native admission is never repeated and performs one cleanup request', async () => {
  const f = fixture(), original = f.deps.request;
  f.deps.request = async op => {
    const result = await original(op);
    if (op === 'escape') result.input.keyUpAdmitted = 0;
    return result;
  };
  const result = await beginNativeFocusRecovery(params, f.deps);
  assert.equal(result.started, false);
  assert.deepEqual(f.calls, [['prepare'], ['focus', 7], ['inspect'], ['escape'], ['finish']]);
});

test('a page change during window activation refuses Escape and restores through the host', async () => {
  const f = fixture();
  f.deps.focusWindow = async id => { f.calls.push(['focus', id]); f.state.focused = true; f.state.tab.url = 'https://example.invalid/other'; };
  const result = await beginNativeFocusRecovery(params, f.deps);
  assert.equal(result.started, false);
  assert.deepEqual(f.calls, [['prepare'], ['focus', 7], ['finish']]);
});

test('a tab switch during native inspection cannot receive Escape', async () => {
  const f = fixture(), original = f.deps.request;
  f.deps.request = async op => {
    const result = await original(op);
    if (op === 'inspect') f.state.tab.active = false;
    return result;
  };
  const result = await beginNativeFocusRecovery(params, f.deps);
  assert.equal(result.started, false);
  assert.deepEqual(f.calls, [['prepare'], ['focus', 7], ['inspect'], ['finish']]);
});

test('preparation returns cleanup authority before any focus or key request', async () => {
  const f = fixture();
  const lease = await prepareNativeFocusRecovery(params, f.deps);
  assert.equal(lease.token, token);
  assert.deepEqual(f.calls, [['prepare']]);
  await lease.finish();
  const result = await lease.begin();
  assert.equal(result.started, false);
  assert.deepEqual(f.calls, [['prepare'], ['finish']]);
});

test('cancellation during native inspection prevents the later key request', async () => {
  const f = fixture(), original = f.deps.request;
  let reached, resume;
  const inspecting = new Promise(resolve => { reached = resolve; });
  const pending = new Promise(resolve => { resume = resolve; });
  f.deps.request = async op => {
    const result = await original(op);
    if (op === 'inspect') { reached(); await pending; }
    return result;
  };
  const lease = await prepareNativeFocusRecovery(params, f.deps);
  const running = lease.begin();
  await inspecting;
  await lease.finish();
  resume();
  const result = await running;
  assert.equal(result.started, false);
  assert.deepEqual(f.calls, [['prepare'], ['focus', 7], ['inspect'], ['finish']]);
});

test('native timeout and late replies never resend a request or enter CDP', async () => {
  const sent = [], timers = [];
  const bridge = new NativeFocusBridge({ identity: 'fixture', send: message => sent.push(message), schedule: callback => { timers.push(callback); return 1; }, cancel: () => {} });
  const request = bridge.request('prepare', { allowFocusRecovery: true, url: params.url });
  timers[0]();
  await assert.rejects(request, /unconfirmed/);
  assert.equal(bridge.receive({ method: 'nativeFocusReply', requestId: sent[0].requestId, result: { schemaVersion: 1, token, leaseMs: 120000 } }), true);
  assert.equal(sent.length, 1);
  assert.equal(bridge.pending.size, 0);
  assert.equal(bridge.receive({ method: 'someCdpMessage' }), false);
});

test('disconnect and malformed admission cannot report a successful native operation', async () => {
  const sent = [];
  const bridge = new NativeFocusBridge({ identity: 'fixture', send: message => sent.push(message), schedule: () => 1, cancel: () => {} });
  const disconnected = bridge.request('escape', { token });
  bridge.disconnect();
  await assert.rejects(disconnected, /disconnected/);
  const malformed = bridge.request('escape', { token });
  bridge.receive({ method: 'nativeFocusReply', requestId: sent.at(-1).requestId, result: { schemaVersion: 1, token, input: { keyDownAdmitted: 2, keyUpAdmitted: 1, delivery: 'verified', replayed: false } } });
  await assert.rejects(malformed, /Malformed/);
  assert.equal(sent.length, 2);
});

test('default timers keep the WorkerGlobalScope receiver required by browser APIs', async () => {
  const savedSet = globalThis.setTimeout, savedClear = globalThis.clearTimeout;
  const sent = [];
  let cleared = false;
  globalThis.setTimeout = function () { assert.equal(this, globalThis); return 123; };
  globalThis.clearTimeout = function (timer) { assert.equal(this, globalThis); assert.equal(timer, 123); cleared = true; };
  try {
    const bridge = new NativeFocusBridge({ identity: 'receiver-fixture', send: message => sent.push(message) });
    const waiting = bridge.request('prepare', { allowFocusRecovery: true, url: params.url });
    bridge.receive({ method: 'nativeFocusReply', requestId: sent[0].requestId, result: { schemaVersion: 1, token, leaseMs: 20000 } });
    assert.equal((await waiting).token, token);
    assert.equal(cleared, true);
  } finally {
    globalThis.setTimeout = savedSet;
    globalThis.clearTimeout = savedClear;
  }
});

test('revocation control does not wait for an outstanding native inspection', async () => {
  const sent = [];
  const bridge = new NativeFocusBridge({ identity: 'revoke-fixture', send: message => sent.push(message), schedule: () => 1, cancel: () => {} });
  const waiting = bridge.request('inspect', { token });
  assert.equal(bridge.revoke(token), true);
  assert.equal(sent[1].method, 'nativeFocusRevoke');
  assert.equal(bridge.pending.size, 1);
  bridge.receive({ method: 'nativeFocusReply', requestId: sent[0].requestId, result: { schemaVersion: 1, token, verified: true } });
  await waiting;
  assert.equal(sent.length, 2);
});

test('a capability probe cannot return an input-enabled or lease-bearing success', async () => {
  const sent = [];
  const bridge = new NativeFocusBridge({ identity: 'probe-fixture', send: message => sent.push(message), schedule: () => 1, cancel: () => {} });
  const waiting = bridge.request('probe', {});
  bridge.receive({ method: 'nativeFocusReply', requestId: sent[0].requestId,
    result: { schemaVersion: 1, platform: 'macos', probeOnly: true, nativeInputEnabled: false,
      preflight: { accessibilityTrusted: false, postEventsAllowed: null, listenEventsAllowed: false } } });
  assert.equal((await waiting).nativeInputEnabled, false);
  const invalid = bridge.request('probe', {});
  bridge.receive({ method: 'nativeFocusReply', requestId: sent[1].requestId,
    result: { schemaVersion: 1, platform: 'macos', probeOnly: true, nativeInputEnabled: true } });
  await assert.rejects(invalid, /Malformed/);
  const leaseBearing = bridge.request('probe', {});
  bridge.receive({ method: 'nativeFocusReply', requestId: sent[2].requestId,
    result: { schemaVersion: 1, platform: 'macos', probeOnly: true, nativeInputEnabled: false, token } });
  await assert.rejects(leaseBearing, /Malformed/);
});
