import assert from 'node:assert/strict';
import test from 'node:test';
import { InspectController, annotationPayload, deliveryLocked, normalizeSettings } from './inspect-controller.js';

function fakeChrome() {
  const store = {};
  const listeners = {};
  const on = name => ({ addListener: fn => { (listeners[name] ||= []).push(fn); } });
  const chrome = {
    runtime: { id: 'ext', getURL: path => `chrome-extension://ext/${path}`, onConnect: on('connect') },
    commands: { onCommand: on('command') },
    tabs: { onRemoved: on('removed'), onActivated: on('activated'), onZoomChange: on('zoom'), query: async () => [], get: async () => ({}) },
    windows: { onRemoved: on('wremoved') },
    webNavigation: { onCommitted: on('committed'), onHistoryStateUpdated: on('history'), getFrame: async () => ({ documentId: 'doc' }) },
    scripting: { injected: [], executeScript: async args => { chrome.scripting.injected.push(args); return [{ result: [] }]; } },
    storage: { local: { get: async key => ({ [key]: store[key] }), set: async value => Object.assign(store, value) } },
  };
  return { chrome, store, listeners };
}
let lastListeners = null;

function framePort(sender) {
  const port = {
    name: 'inspect-frame', sender, sent: [], disconnected: false, msg: [], disc: [],
    postMessage(m) { this.sent.push(m); },
    disconnect() { this.disconnected = true; },
    onMessage: { addListener: fn => port.msg.push(fn) },
    onDisconnect: { addListener: fn => port.disc.push(fn) },
  };
  return port;
}
const flush = () => new Promise(r => setTimeout(r, 0));
const element = { label: 'button "Save"', selector: 'button.save', path: 'button.save', classes: '', rect: { x: 0, y: 0, width: 10, height: 10 }, text: 'Save', selectedText: '', nearby: [], styles: [], html: '<button>Save</button>', source: { kind: 'insp', path: 'src/a.tsx', line: 3, column: 1 } };

async function started() {
  const { chrome, store, listeners } = fakeChrome();
  lastListeners = listeners;
  const calls = [];
  const controller = new InspectController({ chrome, hostReady: () => true, request: async (op, params) => { calls.push([op, params]); return op === 'inspectDeliver' ? { schemaVersion: 1, state: 'sent', wake: 'accepted' } : { schemaVersion: 1 }; } });
  controller.install();
  await controller.start({ id: 7, windowId: 1, url: 'http://localhost:5173/orders?token=x' });
  const picker = { id: 'ext', tab: { id: 7 }, frameId: 0, documentId: 'doc' };
  const hello = await new Promise(resolve => controller.onPickerMessage({ type: 'inspect:hello' }, picker, resolve));
  return { chrome, store, controller, calls, picker, hello };
}

test('start injects the extractor and picker into the top frame only', async () => {
  const { chrome, hello } = await started();
  assert.deepEqual(chrome.scripting.injected[0], { target: { tabId: 7, frameIds: [0] }, files: ['inspect-extract.js', 'inspect-picker.js'] });
  assert.equal(hello.ok, true);
  assert.equal(hello.frameOrigin, 'chrome-extension://ext');
  assert.equal(hello.enabled, false);
});

test('only the frame holding the secret is authorized; others are disconnected', async () => {
  const { controller, hello } = await started();
  const copy = framePort({ id: 'ext', tab: { id: 7 }, frameId: 9, documentId: 'c', url: 'chrome-extension://ext/inspect-frame.html' });
  controller.acceptPort(copy);
  copy.msg[0]({ type: 'auth', secret: 'guess' });
  assert.equal(copy.disconnected, true);
  assert.deepEqual(copy.sent.at(-1), { type: 'denied' });
  const real = framePort({ id: 'ext', tab: { id: 7 }, frameId: 5, documentId: 'f', url: 'chrome-extension://ext/inspect-frame.html' });
  controller.acceptPort(real);
  real.msg[0]({ type: 'auth', secret: hello.secret });
  await flush(); await flush();
  const authorized = real.sent.find(m => m.type === 'authorized');
  assert.equal(authorized.pageUrl, 'http://localhost:5173/orders', 'query string never reaches the frame');
  assert.equal(authorized.env, 'local');
});

test('delivery goes out once per id and refuses a replay of a locked batch', async () => {
  const { controller, hello, calls, picker } = await started();
  const real = framePort({ id: 'ext', tab: { id: 7 }, frameId: 5, documentId: 'f', url: 'chrome-extension://ext/inspect-frame.html' });
  controller.acceptPort(real);
  real.msg[0]({ type: 'auth', secret: hello.secret });
  await flush();
  const instance = controller.registry.get(7);
  instance.annotations = [{ element, probe: null, note: '' }];
  const deliveryId = '11111111-2222-3333-4444-555555555555';
  real.msg[0]({ type: 'deliver', requestId: 1, deliveryId, revision: 0, instruction: 'Round the corners', images: [{ index: 0, dataUrl: 'data:image/png;base64,iVBORw0KGgo=' }, { index: 3, dataUrl: 'data:image/png;base64,AAAA' }] });
  await flush(); await flush(); await flush();
  const deliver = calls.find(c => c[0] === 'inspectDeliver')[1];
  assert.equal(deliver.origin, 'http://localhost:5173');
  assert.equal(deliver.annotations[0].sourcePath, 'src/a.tsx');
  assert.equal(deliver.images.length, 1, 'images for unknown indexes are dropped');
  assert.equal(real.sent.find(m => m.type === 'reply' && m.requestId === 1).result.state, 'sent');
  real.msg[0]({ type: 'deliver', requestId: 2, deliveryId, revision: 0, instruction: 'Round the corners' });
  await flush(); await flush();
  assert.match(real.sent.find(m => m.type === 'error' && m.requestId === 2).error, /already sent/);
  assert.equal(calls.filter(c => c[0] === 'inspectDeliver').length, 1);
  void picker;
});

test('a host failure leaves the batch unconfirmed, not failed', async () => {
  const { controller, hello, store } = await started();
  controller.request = async () => { throw new Error('timeout'); };
  const real = framePort({ id: 'ext', tab: { id: 7 }, frameId: 5, documentId: 'f', url: 'chrome-extension://ext/inspect-frame.html' });
  controller.acceptPort(real);
  real.msg[0]({ type: 'auth', secret: hello.secret });
  await flush();
  controller.registry.get(7).annotations = [{ element, probe: null, note: '' }];
  real.msg[0]({ type: 'deliver', requestId: 1, deliveryId: '11111111-2222-3333-4444-555555555556', revision: 0, instruction: 'x' });
  await flush(); await flush(); await flush();
  assert.equal(real.sent.find(m => m.requestId === 1).result.state, 'unconfirmed');
  assert.equal(store.inspectDrafts['http://localhost:5173'].state, 'unconfirmed');
});

test('pure helpers', () => {
  assert.equal(deliveryLocked('sent'), true);
  assert.equal(deliveryLocked('editing'), false);
  assert.deepEqual(normalizeSettings({ enabled: 'yes', editor: 'emacs' }), { enabled: false, editor: 'vscode' });
  const payload = annotationPayload({ element: { ...element, source: null }, probe: { component: 'Save', ancestors: [], source: { kind: 'react', path: 'src/b.tsx', line: 9, column: null } }, note: 'n' }, 1);
  assert.equal(payload.sourceKind, 'react');
  assert.equal(payload.sourceLine, 9);
  assert.match(payload.markdown, /^### 1\./);
});

async function authorized() {
  const ctx = await started();
  const real = framePort({ id: 'ext', tab: { id: 7 }, frameId: 5, documentId: 'f', url: 'chrome-extension://ext/inspect-frame.html' });
  ctx.controller.acceptPort(real);
  real.msg[0]({ type: 'auth', secret: ctx.hello.secret });
  await flush();
  return { ...ctx, real, instance: ctx.controller.registry.get(7) };
}

test('a late probe result never rolls the selection back', async () => {
  const { controller, chrome, instance } = await authorized();
  controller.queueCapture = () => {};
  const gates = [];
  chrome.scripting.executeScript = () => new Promise(resolve => gates.push(resolve));
  const a = controller.updateSelection(instance, { elements: [{ ...element, selector: 'a' }], probeAttribute: 'data-abcdefghijkl', selectionSeq: 1 });
  const b = controller.updateSelection(instance, { elements: [{ ...element, selector: 'b' }], probeAttribute: 'data-abcdefghijkl', selectionSeq: 2 });
  gates[1]([{ result: [] }]);
  await b;
  gates[0]([{ result: [] }]);
  assert.deepEqual(await a, { ok: true, stale: true });
  assert.equal(instance.annotations[0].element.selector, 'b');
  assert.equal(instance.selectionRevision, 2);
});

test('a capture for an earlier selection is never published', async () => {
  const { controller, chrome, instance, real } = await authorized();
  instance.annotations = [{ id: '1:0', element, probe: null, note: '' }];
  instance.selectionRevision = 1;
  instance.pickerSeq = 1;
  let release;
  chrome.tabs.get = async () => ({ id: 7, windowId: 1, active: true, status: 'complete' });
  chrome.tabs.captureVisibleTab = () => new Promise(resolve => { release = resolve; });
  const layout = { innerWidth: 100, innerHeight: 100, scrollX: 0, scrollY: 0, devicePixelRatio: 1, selectionSeq: 1, rects: [{ x: 0, y: 0, width: 10, height: 10 }] };
  controller.pickerCall = async () => ({ ok: true, layout });
  const capture = controller.queueCapture(instance);
  await flush(); await flush();
  instance.selectionRevision = 2; // the user picked something else meanwhile
  release('data:image/png;base64,AAAA');
  await capture;
  assert.equal(real.sent.filter(m => m.type === 'images').length, 0);
});

test('a capture whose picker layout belongs to a newer selection is dropped', async () => {
  const { controller, chrome, instance, real } = await authorized();
  instance.annotations = [{ id: '1:0', element, probe: null, note: '' }];
  instance.selectionRevision = 1;
  instance.pickerSeq = 1;
  chrome.tabs.get = async () => ({ id: 7, windowId: 1, active: true });
  chrome.tabs.captureVisibleTab = async () => { throw new Error('must not capture'); };
  const calls = [];
  controller.pickerCall = async (_instance, message) => { calls.push(message.type); return { ok: true, layout: { selectionSeq: 2, rects: [{}, {}] } }; };
  await controller.queueCapture(instance);
  assert.deepEqual(calls, ['inspect:prepareCapture', 'inspect:finishCapture'], 'picker UI is restored');
  assert.equal(real.sent.filter(m => m.type === 'images').length, 0);
});

test('send is refused when the selection changed after review', async () => {
  const { instance, real, calls } = await authorized();
  instance.annotations = [{ id: '3:0', element, probe: null, note: '' }];
  instance.selectionRevision = 3;
  real.msg[0]({ type: 'deliver', requestId: 9, deliveryId: '11111111-2222-3333-4444-555555555557', revision: 2, instruction: 'x' });
  await flush(); await flush();
  assert.match(real.sent.find(m => m.requestId === 9).error, /selection changed/);
  assert.equal(calls.filter(c => c[0] === 'inspectDeliver').length, 0);
});

test('drafts: editing text expires, uncertain batches keep only id and state and are never cut', async () => {
  const { pruneDrafts } = await import('./inspect-controller.js');
  const day = 24 * 60 * 60 * 1000;
  const now = 100 * day;
  const drafts = { 'http://old': { instruction: 'secret plan', state: 'editing', updatedAt: now - 8 * day }, 'http://u': { instruction: 'x', notes: { a: 'n' }, deliveryId: 'id', state: 'unknown', updatedAt: now - 9 * day } };
  for (let i = 0; i < 40; i++) drafts['http://e' + i] = { instruction: 'e', state: 'editing', updatedAt: now - i };
  const out = pruneDrafts(drafts, now);
  assert.equal(out['http://old'], undefined);
  assert.deepEqual(out['http://u'], { deliveryId: 'id', state: 'unknown', updatedAt: now - 9 * day });
  assert.equal(Object.keys(out).length, 30);
});

test('a note edited after send started does not change the frozen batch', async () => {
  const { instance, real, calls } = await authorized();
  instance.annotations = [{ id: '0:0', element, probe: null, note: 'old' }];
  instance.selectionRevision = 0;
  real.msg[0]({ type: 'deliver', requestId: 11, deliveryId: '11111111-2222-3333-4444-555555555558', revision: 0, instruction: 'x', notes: { '0:0': 'reviewed note' } });
  real.msg[0]({ type: 'note', requestId: 12, annotationId: '0:0', note: 'edited later' });
  await flush(); await flush(); await flush();
  const sent = calls.find(c => c[0] === 'inspectDeliver')[1];
  assert.equal(sent.annotations[0].note, 'reviewed note');
});

test('matching: status uses inspectResolve with the page path, falls back on old hosts', async () => {
  const ctx = await authorized();
  const { controller, chrome, real, calls, instance } = ctx;
  chrome.tabs.get = async () => ({ id: 7, url: 'http://localhost:5173/admin/orders?x=1' });
  controller.resolveReady = () => true;
  controller.request = async (op, params) => { calls.push([op, params]); return { schemaVersion: 1, state: 'unbound', pendingClaims: [], ocs: { installed: true } }; };
  instance.revision = 'abc1234';
  real.msg[0]({ type: 'status', requestId: 21 });
  await flush(); await flush(); await flush();
  const resolve = calls.find(c => c[0] === 'inspectResolve')[1];
  assert.equal(resolve.pagePath, '/admin/orders');
  assert.equal(resolve.revision, 'abc1234');
  controller.resolveReady = () => false;
  real.msg[0]({ type: 'status', requestId: 22 });
  await flush(); await flush();
  assert.ok(calls.some(c => c[0] === 'inspectStatus'));
  assert.equal(real.sent.find(m => m.requestId === 22).legacy, true);
});

test('matching: a tab-only target is held locally and sent with each delivery', async () => {
  const { controller, chrome, real, calls, instance } = await authorized();
  chrome.tabs.get = async () => ({ id: 7, url: 'http://localhost:5173/shop' });
  controller.resolveReady = () => true;
  const pair = { root: '/w/shop', sourceRoots: ['/w/shop'], sessionId: 'claude-1', sessionCwd: '/w/shop', sessionLabel: 'shop', pathScope: '/shop', fingerprint: 'f'.repeat(32), origin: 'http://localhost:5173', evidence: ['config'] };
  real.msg[0]({ type: 'confirm', requestId: 31, pair, mode: 'tab' });
  await flush();
  assert.equal(calls.filter(c => c[0] === 'inspectBind').length, 0, 'tab target is not persisted');
  assert.equal(instance.tabTarget.fingerprint, pair.fingerprint);
  instance.annotations = [{ id: '0:0', element, probe: null, note: '' }];
  instance.selectionRevision = 0;
  real.msg[0]({ type: 'deliver', requestId: 32, deliveryId: '11111111-2222-3333-4444-555555555559', revision: 0, instruction: 'x', expectedPath: '/shop', expectedTarget: pair.fingerprint });
  await flush(); await flush(); await flush();
  const deliver = calls.find(c => c[0] === 'inspectDeliver')[1];
  assert.equal(deliver.pagePath, '/shop');
  assert.deepEqual(deliver.target, { root: '/w/shop', sourceRoots: ['/w/shop'], sessionId: 'claude-1', sessionCwd: '/w/shop', fingerprint: 'f'.repeat(32) });
  real.msg[0]({ type: 'confirm', requestId: 33, pair, mode: 'default' });
  await flush(); await flush();
  const bind = calls.find(c => c[0] === 'inspectBind')[1];
  assert.equal(bind.pathScope, '/shop');
  assert.equal(bind.fingerprint, pair.fingerprint);
  assert.equal(instance.tabTarget, null, 'site default replaces the tab override');
});

test('matching: malformed pairs and claims are refused before the host', async () => {
  const { controller, real, calls } = await authorized();
  controller.resolveReady = () => true;
  real.msg[0]({ type: 'confirm', requestId: 41, pair: { root: '/w', sessionId: 'x', sourceRoots: [] }, mode: 'default' });
  real.msg[0]({ type: 'claimDecide', requestId: 42, claimId: 5 });
  await flush(); await flush();
  assert.match(real.sent.find(m => m.requestId === 41).error, /Unknown target/);
  assert.match(real.sent.find(m => m.requestId === 42).error, /Unknown claim/);
  assert.equal(calls.filter(c => ['inspectBind', 'inspectClaimDecide'].includes(c[0])).length, 0);
});

test('matching: deliver refuses when the page path or the shown target moved', async () => {
  const { controller, chrome, real, calls, instance } = await authorized();
  controller.resolveReady = () => true;
  let url = 'http://localhost:5173/admin';
  chrome.tabs.get = async () => ({ id: 7, url });
  instance.annotations = [{ id: '0:0', element, probe: null, note: '' }];
  instance.selectionRevision = 0;
  url = 'http://localhost:5173/shop';
  real.msg[0]({ type: 'deliver', requestId: 51, deliveryId: '11111111-2222-3333-4444-555555555560', revision: 0, instruction: 'x', expectedPath: '/admin', expectedTarget: 'a'.repeat(32) });
  await flush(); await flush(); await flush();
  assert.deepEqual(real.sent.find(m => m.requestId === 51).result, { state: 'targetChanged', reason: 'pageChanged' });
  real.msg[0]({ type: 'deliver', requestId: 52, deliveryId: '11111111-2222-3333-4444-555555555561', revision: 0, instruction: 'x', expectedPath: '/shop' });
  await flush(); await flush(); await flush();
  assert.equal(real.sent.find(m => m.requestId === 52).result.reason, 'bindingChanged');
  assert.equal(calls.filter(c => c[0] === 'inspectDeliver').length, 0, 'nothing reaches the host');
  real.msg[0]({ type: 'deliver', requestId: 53, deliveryId: '11111111-2222-3333-4444-555555555562', revision: 0, instruction: 'x', expectedPath: '/shop', expectedTarget: 'b'.repeat(32) });
  await flush(); await flush(); await flush();
  const sent = calls.find(c => c[0] === 'inspectDeliver')[1];
  assert.equal(sent.pagePath, '/shop');
  assert.equal(sent.pageUrl, 'http://localhost:5173/shop', 'pageUrl and pagePath come from one snapshot');
  assert.equal(sent.expectedTarget, 'b'.repeat(32));
});

test('matching: a route change clears the tab target and tells the panel', async () => {
  const { real, instance } = await authorized();
  instance.tabTarget = { fingerprint: 'f' };
  lastListeners.history[0]({ tabId: 7, frameId: 0, url: 'http://localhost:5173/shop?q=1' });
  assert.equal(instance.tabTarget, null);
  assert.deepEqual(real.sent.at(-1), { type: 'pageChanged', url: 'http://localhost:5173/shop' });
  lastListeners.history[0]({ tabId: 7, frameId: 3, url: 'http://localhost:5173/x' });
  assert.equal(real.sent.at(-1).url, 'http://localhost:5173/shop', 'subframe history is ignored');
});

test('matching: a refused precheck never records the batch as sending', async () => {
  const { controller, chrome, real, calls, instance, store } = await authorized();
  controller.resolveReady = () => true;
  chrome.tabs.get = async () => ({ id: 7, url: 'http://localhost:5173/shop' });
  instance.annotations = [{ id: '0:0', element, probe: null, note: 'n' }];
  instance.selectionRevision = 0;
  const id = '11111111-2222-3333-4444-555555555563';
  real.msg[0]({ type: 'deliver', requestId: 61, deliveryId: id, revision: 0, instruction: 'x', expectedPath: '/admin', expectedTarget: 'a'.repeat(32) });
  await flush(); await flush(); await flush();
  assert.equal(real.sent.find(m => m.requestId === 61).result.reason, 'pageChanged');
  assert.notEqual(store.inspectDrafts?.['http://localhost:5173']?.state, 'sending', 'not stuck as sending');
  // Reopened and re-checked: the same id may now be sent.
  real.msg[0]({ type: 'deliver', requestId: 62, deliveryId: id, revision: 0, instruction: 'x', expectedPath: '/shop', expectedTarget: 'a'.repeat(32) });
  await flush(); await flush(); await flush();
  assert.equal(real.sent.find(m => m.requestId === 62).result.state, 'sent');
  assert.equal(calls.filter(c => c[0] === 'inspectDeliver').length, 1);
});

test('the frame reads its batch replies only from a host that keeps them', async () => {
  const ctx = await authorized();
  const { real, controller, calls } = ctx;
  const deliveryId = '11111111-2222-4333-8444-555555555555';
  real.msg[0]({ type: 'replies', requestId: 41, deliveryId });
  await flush();
  assert.deepEqual(real.sent.find(m => m.requestId === 41).replies, null);
  assert.equal(calls.filter(c => c[0] === 'inspectReplies').length, 0);
  controller.repliesReady = () => true;
  controller.request = async (op, params) => { calls.push([op, params]); return { schemaVersion: 1, replies: [{ channel: 'dm-a', seq: 2 }] }; };
  real.msg[0]({ type: 'replies', requestId: 42, deliveryId });
  await flush();
  assert.deepEqual(calls.at(-1), ['inspectReplies', { deliveryId }]);
  assert.deepEqual(real.sent.find(m => m.requestId === 42).replies, [{ channel: 'dm-a', seq: 2 }]);
  real.msg[0]({ type: 'replies', requestId: 43, deliveryId: '../x' });
  await flush();
  assert.equal(real.sent.find(m => m.requestId === 43).type, 'error');
});
