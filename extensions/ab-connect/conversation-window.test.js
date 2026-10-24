import { loadMessages } from './test-i18n.js';
loadMessages('en');
import assert from 'node:assert/strict';
import test from 'node:test';
import { createConversationWindows } from './conversation-window.js';

function fixture() {
  const calls = [], live = new Map([[21, { id: 21, windowId: 7, url: 'https://example.com/task', active: false }]]);
  const saved = { local: {}, session: {} };
  const storage = Object.fromEntries(Object.entries(saved).map(([area, data]) => [area, { async get() { return structuredClone(data); }, async set(value) { Object.assign(data, structuredClone(value)); } }]));
  const tabs = {
    async get(id) { if (!live.has(id)) throw new Error(`No tab with id: ${id}`); return { ...live.get(id) }; },
    async create(options) { calls.push(['create', options]); const tab = { id: Math.max(21, ...live.keys()) + 1, windowId: 8, ...options }; live.set(tab.id, tab); return tab; },
    async update(id, options) { calls.push(['activate', id]); Object.assign(live.get(id), options); },
  };
  const windows = { async update(id) { calls.push(['focus', id]); }, async get() { return { focused: true }; } };
  const make = () => createConversationWindows({ tabs, windows, storage, boundTab: async () => null });
  return { api: make(), make, calls, live, saved, tabs, windows };
}

test('focuses the exact observed conversation tab and verifies both tab and window', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  assert.deepEqual(await f.api.show('a'), { tabId: 21, windowId: 7 });
  assert.deepEqual(f.calls, [['activate', 21], ['focus', 7]]);
});
test('closed pages reopen at their last URL; concurrent clicks create once', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  f.live.get(21).url = 'https://example.com/next'; await f.api.updated(f.live.get(21)); f.live.delete(21);
  await Promise.all([f.api.show('a'), f.api.show('a')]);
  assert.equal(f.calls.filter(([call]) => call === 'create').length, 1);
  assert.equal(f.calls[0][1].url, 'https://example.com/next');
  await f.api.show('a'); assert.equal(f.calls.filter(([call]) => call === 'create').length, 1);
});
test('browser restart keeps restore URL but never trusts old tab ids', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  const storage = { local: { async get() { return f.saved.local; }, async set() {} }, session: { async get() { return {}; }, async set() {} } };
  const api = createConversationWindows({ tabs: f.tabs, windows: f.windows, storage, boundTab: async () => null });
  await api.show('a'); assert.equal(f.calls[0][0], 'create');
});
test('unreadable live tab does not authorize duplicate creation', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  f.tabs.get = async () => { throw new Error('Chrome is unavailable'); };
  await assert.rejects(f.api.show('a'), /Chrome is unavailable/); assert.deepEqual(f.calls, []);
});
test('failed focus is reported and retry reuses the newly restored tab', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21); f.live.delete(21);
  f.windows.get = async () => ({ focused: false });
  await assert.rejects(f.api.show('a'), /Could not confirm/);
  await assert.rejects(f.api.show('a'), /Could not confirm/);
  assert.equal(f.calls.filter(([call]) => call === 'create').length, 1);
});
test('no destination and unsafe URLs refuse without opening any page', async () => {
  const f = fixture(); await assert.rejects(f.api.show('missing'), /No saved webpage/);
  f.live.get(21).url = 'javascript:alert(1)'; await f.api.observe('panel-a', 21); f.live.delete(21);
  await assert.rejects(f.api.show('a'), /No saved webpage/); assert.deepEqual(f.calls, []);
});
test('ordinary sessions are ignored and panel binding is a fallback for existing chats', async () => {
  const f = fixture(); await f.api.observe('terminal-a', 21);
  await assert.rejects(f.api.show('a'), /No saved webpage/);
  const api = createConversationWindows({ tabs: f.tabs, windows: f.windows, storage: { local: { async get() { return {}; }, async set() {} }, session: { async get() { return {}; }, async set() {} } }, boundTab: async () => 21 });
  assert.equal((await api.show('a')).tabId, 21);
});

 test('selecting a chat on another page never replaces its actual agent destination', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  await f.api.binding('a', { id: 30, url: 'https://other.example', windowId: 10 });
  assert.equal((await f.api.show('a')).tabId, 21);
 });
 test('Chrome tab replacement follows the new id without reopening', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  f.live.set(23, { id: 23, windowId: 7, url: 'https://example.com/new', active: false });
  await f.api.replaced(21, f.live.get(23)); f.live.delete(21);
  assert.equal((await f.api.show('a')).tabId, 23);
  assert.equal(f.calls.some(([kind]) => kind === 'create'), false);
 });

 test('an empty tab response preserves the restore URL even after focus fails', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21); f.live.delete(21);
  f.tabs.create = async options => { f.calls.push(['create', options]); const tab={id:22,windowId:8,active:false}; f.live.set(22,tab); return tab; };
  f.windows.get = async () => ({ focused: false });
  await assert.rejects(f.api.show('a'), /Could not confirm/); f.live.delete(22);
  await assert.rejects(f.api.show('a'), /Could not confirm/);
  assert.equal(f.calls.filter(([kind]) => kind === 'create')[1][1].url, 'https://example.com/task');
 });
 test('deleting a conversation removes its saved page', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21); await f.api.forget('a');
  await assert.rejects(f.make().show('a'), /No saved webpage/);
 });

test('a late relay observation cannot restore data after deletion', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  let resolve; f.tabs.get = () => new Promise(r => { resolve=r; });
  const observing=f.api.observe('panel-a',21);
  await f.api.forget('a');
  resolve({id:21,url:'https://example.com/private'}); await observing;
  await assert.rejects(f.make().show('a'), /No saved webpage/);
});

// Exercise the shipped background handler, including its asynchronous reply.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
const background = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
function deleteHandler(forget, result) {
  const start=background.indexOf("  if (msg?.type === 'ab-agent-request') {");
  const end=background.indexOf('  // Version banners',start);
  assert.ok(start>=0&&end>start);
  return vm.runInNewContext(`(function(msg,_sender,sendResponse){${background.slice(start,end)}})`, {
    chrome:{runtime:{}}, isAgentPanelSender:()=>true, pageOperationAllowed:()=>true,
    agentPanelBridge:{request:async()=>result}, conversationWindows:{forget},
  });
}
test('deletion waits for saved page cleanup and reports partial failure', async () => {
  let resolve; const pending=new Promise(r=>{resolve=r;}); let response;
  const result={deleted:{id:'a'}};
  const handler=deleteHandler(()=>pending,result);
  handler({type:'ab-agent-request',operation:'conversationDelete'}, {}, reply=>{response=reply;});
  await new Promise(r=>setImmediate(r)); assert.equal(response,undefined);
  resolve();await new Promise(r=>setImmediate(r));assert.equal(response.success,true);
  const failed=deleteHandler(async()=>{throw new Error('storage unavailable');},{deleted:{id:'a'}});
  const reply=await new Promise(r=>failed({type:'ab-agent-request',operation:'conversationDelete'}, {}, r));
  assert.equal(reply.result.deleted.id,'a');
  assert.equal(reply.result.deleted.pageCleanup.error,'storage unavailable');
});


test('initial and final destinations stay distinct across navigation, reopening and restart', async () => {
  const f = fixture();
  await f.api.binding('a', f.live.get(21));
  f.live.get(21).url = 'https://example.com/result'; await f.api.observe('panel-a', 21);
  await Promise.all([f.api.show('a', 'initial'), f.api.show('a', 'initial')]);
  assert.equal(f.calls.filter(([kind]) => kind === 'create').length, 1);
  assert.equal(f.calls[0][1].url, 'https://example.com/task');
  assert.equal(f.saved.local.ab_conversation_pages.a.url, 'https://example.com/result');
  assert.equal((await f.api.show('a', 'final')).tabId, 21);
  await f.make().show('a', 'initial');
  assert.equal(f.calls.filter(([kind]) => kind === 'create').length, 1);
  f.saved.session = {}; // Simulate browser restart with only persistent URLs.
  const restarted = createConversationWindows({ tabs: f.tabs, windows: f.windows,
    storage: { local: { async get() { return f.saved.local; }, async set() {} }, session: { async get() { return {}; }, async set() {} } }, boundTab: async () => null });
  await restarted.show('a', 'initial');
  assert.equal(f.calls.filter(([kind]) => kind === 'create').at(-1)[1].url, 'https://example.com/task');
  await restarted.show('a', 'final');
  assert.equal(f.calls.filter(([kind]) => kind === 'create').at(-1)[1].url, 'https://example.com/result');
});

test('legacy records never invent an initial destination and invalid selectors refuse', async () => {
  const f = fixture();
  f.saved.local.ab_conversation_pages = { a: { url: 'https://example.com/latest' } };
  await assert.rejects(f.api.show('a', 'initial'), /No initial webpage/);
  await assert.rejects(f.api.show('a', 'typo'), /Unknown webpage destination/);
  assert.deepEqual(f.calls, []);
  await f.api.show('a', 'final');
  assert.equal(f.calls[0][1].url, 'https://example.com/latest');
});

test('a new conversation initially bound to a blank tab records its first web URL', async () => {
  const f = fixture();
  await f.api.binding('a', { id: 21, url: 'about:blank' });
  f.live.get(21).url = 'https://example.com/start'; await f.api.updated(f.live.get(21));
  assert.equal((await f.api.show('a', 'initial')).tabId, 21);
  assert.equal(f.saved.local.ab_conversation_pages.a.initialUrl, 'https://example.com/start');
});


test('explicit final destination does not focus a tab that moved to an unrelated URL', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  f.live.get(21).url = 'https://other.example/unrecorded';
  const result = await f.api.show('a', 'final');
  assert.notEqual(result.tabId, 21);
  assert.equal(f.calls[0][1].url, 'https://example.com/task');
});

test('explicit navigation reports uncertainty when Chrome returns the wrong destination', async () => {
  const f = fixture(); await f.api.observe('panel-a', 21);
  f.tabs.update = async id => { Object.assign(f.live.get(id), { active: true, url: 'https://other.example/' }); };
  await assert.rejects(f.api.show('a', 'initial'), /Could not confirm/);
});
