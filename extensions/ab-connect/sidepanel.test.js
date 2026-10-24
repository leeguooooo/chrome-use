import { loadMessages } from './test-i18n.js';
const en = loadMessages('en');
import assert from 'node:assert/strict';
import test from 'node:test';
import { createPanel, slashQuery } from './sidepanel.js';

// A deliberately small DOM fixture: controller tests verify asynchronous
// conversation identity, not browser layout or extension API compatibility.
class Node {
  constructor(tag = 'div') {
    this.tag = tag; this.children = []; this.listeners = {}; this.dataset = {};
    this.value = ''; this.textContent = ''; this.hidden = false; this.disabled = false;
    this.scrollHeight = 100; this.clientHeight = 100; this.scrollTop = 0;
    this.classList = { toggle() {} };
  }
  append(...nodes) { for (const node of nodes) { node.parent = this; this.children.push(node); } }
  insertBefore(node, before) { if (node.parent) node.remove(); node.parent=this; const index=this.children.indexOf(before); this.children.splice(index<0?this.children.length:index,0,node); }
  replaceChildren(...nodes) { this.children = []; this.append(...nodes); }
  get firstElementChild() { return this.children[0]; }
  addEventListener(type, listener) { this.listeners[type] = listener; }
  setAttribute() {}
  querySelectorAll(tag) { return this.children.flatMap(node => [ ...(node.tag === tag ? [node] : []), ...node.querySelectorAll(tag)]); }
  remove() { this.parent.children = this.parent.children.filter(node => node !== this); }
  fire(type, details = {}) { this.listeners[type]?.({ preventDefault() {}, ...details }); }
}
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const settle = () => new Promise(resolve => setImmediate(resolve));
const fullSupport = {schemaVersion:1,mode:'full',modelInspection:true};
const chat = (id, phase = 'idle') => ({ id, title: id, phase, queued: 0, queuePaused: false, model: '' });

async function fixture(overrides = {}, saved = {}) {
  const nodes = new Map(), calls = [], writes = [];
  let hostState;
  const chats = [chat('a', 'running'), chat('b')];
  const presence = { posted: [], listeners: {} };
  const runtime = {
    ...(overrides.getURL ? { getURL: overrides.getURL } : {}),
    onMessage: { addListener(listener) { hostState = listener; } },
    ...(overrides.presence ? { connect({ name }) {
      presence.name = name;
      return { postMessage(message) { presence.posted.push(message); }, onMessage: { addListener(listener) { presence.listeners.message = listener; } }, onDisconnect: { addListener(listener) { presence.listeners.disconnect = listener; } }, disconnect() {} };
    } } : {}),
    async sendMessage(request) {
      if (request.type === 'ab-status') return overrides.status ? {agentPanel:fullSupport,...overrides.status()} : { connected: true,agentPanel:fullSupport };
      calls.push(request);
      if (request.type === 'ab-conversation-window') return { success: false };
      if (request.type === 'ab-tab-grant') return overrides.tabGrant ? overrides.tabGrant(request) : { success: true, grants: [] };
      if (request.type?.startsWith?.('ab-inspect-pick')) return overrides.pick ? overrides.pick(request) : { success: true, started: true };
      if (overrides[request.operation]) return overrides[request.operation](request.params);
      let result;
      switch (request.operation) {
        case 'discover': result = { agents: [] }; break;
        case 'conversationList': result = { conversations: chats }; break;
        case 'conversationRead': result = { conversation: chats.find(c => c.id === request.params.conversationId), journal: { events: [], cursor: 0, more: false } }; break;
        case 'conversationMessageStatus': result = { message: { known: false } }; break;
        default: throw new Error(`Unexpected operation ${request.operation}`);
      }
      return { success: true, result };
    },
  };
  const document = { createElement: tag => new Node(tag), createTextNode: text => Object.assign(new Node('#text'), { textContent: text }), getElementById(id) { if (!nodes.has(id)) nodes.set(id, new Node()); return nodes.get(id); }, listeners: {}, addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); } };
  for (const id of overrides.hiddenNodes || []) document.getElementById(id).hidden = true;
  const windows = overrides.presence || overrides.mode === 'tab' ? { async getCurrent() { return { id: 7 }; } } : undefined;
  const locations = [];
  const session = overrides.session ? { data: overrides.session, listeners: [],
    async get(key) { return key === null ? { ...this.data } : key in this.data ? { [key]: this.data[key] } : {}; },
    async set(values) { Object.assign(this.data, values); },
    async remove(keys) { for (const key of [].concat(keys)) delete this.data[key]; } } : null;
  let storageChanged = null;
  const panel = createPanel({ document, runtime, windows, permissions: overrides.permissions, mode: overrides.mode, initialConversation: overrides.initialConversation, panelTab: overrides.panelTab, setLocation: id => locations.push(id), encodeImage: overrides.encodeImage, tabs: overrides.tabs, storage: { local: { async get() { return saved; }, async set(values) { writes.push(structuredClone(values)); } }, ...(overrides.sync ? { sync: { async get(defaults) { return { ...defaults, ...overrides.sync }; } } } : {}), ...(session ? { session, onChanged: { addListener(fn) { storageChanged = fn; } } } : {}) }, schedule() {}, cancel() {} });
  await panel.start(); await settle();
  return { panel, nodes, calls, chats, writes, presence, locations, session, document, storageChange(changes) { storageChanged(changes, 'session'); }, grantUpdate(grants) { hostState({ type: 'ab-tab-grants', grants }); }, hostUpdate(state) { hostState({type:'ab-host-state',agentPanel:fullSupport,...state}); } };
}

test('a late stop acknowledgement cannot replace the selected conversation', async () => {
  const stop = deferred();
  const f = await fixture({ conversationStop: () => stop.promise });
  f.panel.select('a'); await settle(); f.nodes.get('stop').fire('click');
  f.panel.select('b'); await settle();
  stop.resolve({ success: true, result: { conversation: chat('a', 'stopping') } }); await settle();
  assert.equal(f.nodes.get('phase').textContent, 'Ready');
  assert.equal(f.nodes.get('conversation').value, 'b');
  f.panel.close();
});

test('an initially disconnected panel loads discovery and restores its saved chat after host confirmation', async () => {
  let connected = false;
  const f = await fixture({status:()=>({connected})}, {ab_panel_conversation:'b'});
  assert.equal(f.calls.length,0);
  connected = true; f.hostUpdate({connected:true}); await settle(); await settle();
  assert.ok(f.calls.some(call=>call.operation==='discover'));
  assert.equal(f.nodes.get('conversation').value,'b');
  assert.equal(f.nodes.get('send').disabled,false);f.panel.close();
});

test('legacy and read-only hosts never enable conversations or persistence operations', async () => {
  for (const support of [null,{schemaVersion:1,mode:'readOnly',modelInspection:false}]) {
    const f=await fixture({status:()=>({connected:true,agentPanel:support})});
    assert.equal(f.nodes.get('send').disabled,true);
    assert.equal(f.nodes.get('inspectAgent').disabled,true);
    assert.equal(f.calls.some(call=>call.operation.startsWith('conversation')),false);
    assert.equal(f.calls.some(call=>call.operation==='discover'),Boolean(support));
    f.nodes.get('message').value='not sent';await assert.rejects(f.panel.send(),/supported current CLI/);f.panel.close();
  }
});

test('MCP approvals show arguments and send a once-only decision without persistence metadata', async () => {
  const params={serverName:'chrome_use',mode:'form',message:'Allow browser navigation?',_meta:{codex_approval_kind:'mcp_tool_call',tool_params:{url:'http://127.0.0.1/fixture'},tool_description:'Navigate'},requestedSchema:{type:'object',properties:{}}};
  const f=await fixture({conversationRead:async()=>({success:true,result:{conversation:chat('a','waitingInput'),journal:{events:[{sequence:1,kind:'request',data:{requestId:1,method:'mcpServer/elicitation/request',params}}],cursor:1,more:false}}}),conversationRespond:async()=>({success:true,result:{}})});
  f.panel.select('a');await settle();
  assert.match(f.nodes.get('requests').querySelectorAll('pre')[0].textContent,/127.0.0.1/);
  const allow=f.nodes.get('requests').querySelectorAll('button').find(button=>button.textContent==='Allow once');
  allow.fire('click');await settle();
  assert.deepEqual(f.calls.find(call=>call.operation==='conversationRespond').params.result,{action:'accept',content:{}});f.panel.close();
});

test('sending locks selection until the acknowledgement and keeps the message identity on failure', async () => {
  const send = deferred();
  const f = await fixture({ conversationSend: () => send.promise });
  f.panel.select('a'); await settle(); f.nodes.get('message').value = 'fixture message';
  const sending = f.panel.send();
  assert.equal(f.nodes.get('conversation').disabled, true);
  f.panel.select('b'); assert.equal(f.nodes.get('conversation').value, 'a');
  send.reject(new Error('Host disconnected')); await assert.rejects(sending, /disconnected/);
  assert.equal(f.nodes.get('conversation').disabled, false);
  assert.equal(f.nodes.get('send').disabled, true);
  assert.match(f.nodes.get('queue').textContent, /unconfirmed/);
  f.panel.select('b'); await settle(); assert.equal(f.nodes.get('send').disabled, false);
  f.panel.select('a'); await settle(); assert.equal(f.nodes.get('send').disabled, true);
  const message = f.calls.find(c => c.operation === 'conversationSend');
  const status = f.calls.find(c => c.operation === 'conversationMessageStatus');
  assert.equal(status.params.messageId, message.params.messageId);
  await f.panel.send(); assert.equal(f.calls.filter(c => c.operation === 'conversationSend').length, 1);
  f.panel.close();
});

test('switching conversations preserves each unsent draft', async () => {
  const f = await fixture(); f.panel.select('a'); await settle();
  f.nodes.get('message').value = 'draft a'; f.panel.select('b'); await settle();
  assert.equal(f.nodes.get('message').value, ''); f.nodes.get('message').value = 'draft b';
  f.panel.select('a'); await settle(); assert.equal(f.nodes.get('message').value, 'draft a');
  f.panel.select('b'); await settle(); assert.equal(f.nodes.get('message').value, 'draft b'); f.panel.close();
});

test('an old transcript reply is discarded after conversation selection changes', async () => {
  const read = deferred();
  const f = await fixture({ conversationRead: ({ conversationId }) => conversationId === 'a' ? read.promise : Promise.resolve({ success: true, result: { conversation: chat('b'), journal: { events: [], cursor: 0, more: false } } }) });
  f.panel.select('a'); f.panel.select('b');
  read.resolve({ success: true, result: { conversation: chat('a'), journal: { events: [{ sequence: 1, kind: 'user', data: { messageId: 'old', text: 'old chat' } }], cursor: 1, more: false } } }); await settle();
  assert.equal(f.panel.timeline().rows.length, 0); assert.equal(f.nodes.get('conversation').value, 'b'); f.panel.close();
});


test('reopening a panel checks an uncertain stored ID without resending it', async () => {
  const saved = { ab_panel_conversation: 'a', ab_panel_pending: { a: 'original-message-id' } };
  const f = await fixture({}, saved);
  assert.equal(f.nodes.get('send').disabled, true);
  assert.match(f.nodes.get('queue').textContent, /unconfirmed/);
  assert.ok(f.calls.some(c => c.operation === 'conversationMessageStatus' && c.params.messageId === 'original-message-id'));
  f.nodes.get('message').value = 'do not resend'; await f.panel.send();
  assert.equal(f.calls.filter(c => c.operation === 'conversationSend').length, 0); f.panel.close();
});

test('a durable acceptance clears only the stored uncertain ID', async () => {
  const f = await fixture({ conversationMessageStatus: async () => ({ success: true, result: { message: { known: true } } }) }, { ab_panel_conversation: 'a', ab_panel_pending: { a: 'accepted', b: 'other' } });
  assert.ok(f.writes.some(w => w.ab_panel_pending?.b === 'other' && !w.ab_panel_pending.a));
  assert.equal(f.nodes.get('send').disabled, false);
  assert.equal(f.calls.filter(c => c.operation === 'conversationSend').length, 0); f.panel.close();
});

test('editing a queued message preserves the composer draft and sends its original text for conflict detection', async () => {
  let queued = [{ id: 'queued', text: 'original', truncated: false }, { id: 'second', text: 'second message' }];
  const f = await fixture({
    conversationRead: async () => ({ success:true,result:{ conversation:chat('a','running'), journal:{events:[],cursor:0,more:false},queuedMessages:queued } }),
    conversationQueueEdit: async params => { queued=[{id:params.replacementId,text:params.text},queued[1]]; return {success:true,result:{}}; },
  });
  f.panel.select('a'); await settle(); f.nodes.get('message').value='separate draft';
  const section=f.nodes.get('queuedMessages'); const input=section.querySelectorAll('textarea')[0]; input.value='replacement';
  section.querySelectorAll('button')[0].fire('click'); await settle();
  const edit=f.calls.find(call=>call.operation==='conversationQueueEdit');
  assert.equal(edit.params.messageId,'queued'); assert.equal(edit.params.expectedText,'original');
  assert.equal(edit.params.text,'replacement'); assert.notEqual(edit.params.replacementId,'queued');
  assert.equal(f.nodes.get('message').value,'separate draft');
  assert.equal(section.querySelectorAll('textarea')[0].value,'replacement');
  assert.equal(section.querySelectorAll('textarea')[1].value,'second message'); f.panel.close();
});

test('a truncated queue preview cannot be changed until its full text is read', async () => {
  const full='x'.repeat(1500);
  const f=await fixture({
    conversationRead:async()=>({success:true,result:{conversation:chat('a'),journal:{events:[],cursor:0,more:false},queuedMessages:[{id:'large',text:full.slice(0,1024),truncated:true}]}}),
    conversationQueueMessageRead:async()=>({success:true,result:{queuedMessage:{id:'large',text:full}}}),
  });
  f.panel.select('a'); await settle();
  const section=f.nodes.get('queuedMessages'); const input=section.querySelectorAll('textarea')[0];
  assert.equal(input.readOnly,true);
  section.querySelectorAll('button')[0].fire('click'); await settle();
  assert.equal(input.value,full); assert.equal(input.readOnly,false);
  assert.equal(f.calls.filter(c=>c.operation==='conversationQueueEdit').length,0); f.panel.close();
});

test('direct sending is acknowledged as waiting for stop, and cannot replace a pending direct message', async () => {
  const reply=deferred();
  let current=chat('a','running');
  const f=await fixture({conversationSendDirect:()=>reply.promise,conversationRead:async()=>({success:true,result:{conversation:current,journal:{events:[],cursor:0,more:false},queuedMessages:current.directMessageId?[{id:current.directMessageId,text:'send after stop',truncated:false,direct:true}]:[]}})});
  f.panel.select('a');await settle();f.nodes.get('message').value='send after stop';
  const sending=f.panel.send(true);await settle();
  assert.equal(f.nodes.get('sendDirect').disabled,true);
  const request=f.calls.find(c=>c.operation==='conversationSendDirect');
  assert.ok(f.writes.some(w=>w.ab_panel_pending?.a===request.params.messageId));
  current={...chat('a','stopping'),directMessageId:request.params.messageId,queuePaused:true,queued:1};
  reply.resolve({success:true,result:{conversation:current}});
  await sending;
  assert.equal(f.nodes.get('phase').textContent,'Waiting for stop confirmation');
  assert.equal(f.nodes.get('sendDirect').disabled,true);
  f.nodes.get('message').value='do not replace the first direct message';
  await assert.rejects(f.panel.send(true),/already waiting/);
  assert.equal(f.calls.filter(c=>c.operation==='conversationStop').length,0);
  assert.equal(f.calls.filter(c=>c.operation==='conversationSendDirect').length,1);
  f.panel.close();
});

test('late model metadata cannot populate another agent selection', async () => {
  const probe=deferred();const f=await fixture({agentInspect:()=>probe.promise});
  f.nodes.get('agent').value='claude';f.nodes.get('agent').fire('change');
  probe.resolve({success:true,result:{inspection:{agentId:'codex',models:[{id:'wrong-model',label:'Wrong agent model',efforts:[]}],account:{status:'unverified'},complete:true}}});await settle();
  assert.deepEqual(f.nodes.get('model').children.map(o=>o.value),['']);assert.equal(f.nodes.get('model').value,'');f.panel.close();
});

test('the model menu comes from the CLI catalog, keeps Default first, and a custom ID overrides it', async () => {
  const created=[];
  const f=await fixture({agentInspect:async()=>({success:true,result:{inspection:{agentId:'codex',versionReported:'0.0.1',models:[{id:'suggestion',label:'Suggestion',efforts:['high']}],account:{status:'reportedSignedIn'},complete:false}}}),
    conversationCreate:params=>{created.push(params);return{success:true,result:{conversation:chat('n')}};},conversationSend:()=>({success:true,result:{conversation:chat('n','running')}})});
  f.nodes.get('agent').value='codex';f.nodes.get('agent').fire('change');await settle();
  assert.deepEqual(f.nodes.get('model').children.map(o=>o.value),['','suggestion']);
  assert.equal(f.calls.filter(call=>call.operation==='agentInspect').length,1);
  assert.match(f.nodes.get('modelStatus').textContent,/partial catalog/);assert.match(f.nodes.get('modelStatus').textContent,/execution access is unverified/);
  f.nodes.get('model').value='suggestion';f.nodes.get('customModel').value='manual-model';f.nodes.get('message').value='go';
  await f.panel.send();await settle();
  assert.equal(created[0].model,'manual-model');f.panel.close();
});

test('uncertain sends can be retried explicitly with the same ID and text', async () => {
  let attempts=0;
  const f=await fixture({conversationSend:({conversationId})=>{
    if(++attempts===1) throw new Error('lost acknowledgement');
    return {success:true,result:{conversation:chat(conversationId)}};
  }});
  f.panel.select('b');await settle();f.nodes.get('message').value='retry text';
  await assert.rejects(f.panel.send(),/lost acknowledgement/);
  await f.panel.refresh();await settle();
  assert.equal(f.nodes.get('pendingRecovery').hidden,false);
  assert.equal(f.nodes.get('retryPending').disabled,false);
  f.nodes.get('retryPending').fire('click');await settle();await settle();
  const sends=f.calls.filter(c=>c.operation==='conversationSend');
  assert.equal(sends.length,2);assert.deepEqual(sends[0].params,sends[1].params);
  assert.equal(f.nodes.get('send').disabled,false);f.panel.close();
});

test('restored uncertain IDs can be cleared without resending or claiming cancellation', async () => {
  const f=await fixture({}, {ab_panel_conversation:'b',ab_panel_pending:{b:'stored-id'}});
  assert.equal(f.nodes.get('pendingRecovery').hidden,false);
  assert.equal(f.nodes.get('retryPending').disabled,true);
  f.nodes.get('clearPending').fire('click');await settle();await settle();
  assert.equal(f.calls.some(c=>c.operation==='conversationSend'),false);
  assert.equal(f.nodes.get('send').disabled,false);
  assert.deepEqual(f.writes.at(-1).ab_panel_pending,{});f.panel.close();
});

test('explicit recovery preserves a lost direct-send intent', async () => {
  let attempts=0;
  const f=await fixture({conversationSendDirect:({conversationId})=>{
    if(++attempts===1)throw new Error('lost direct acknowledgement');
    return {success:true,result:{conversation:chat(conversationId)}};
  }});
  f.panel.select('a');await settle();f.nodes.get('message').value='priority text';
  await assert.rejects(f.panel.send(true),/lost direct acknowledgement/);
  await f.panel.refresh();await settle();f.nodes.get('retryPending').fire('click');await settle();await settle();
  const sends=f.calls.filter(c=>c.operation==='conversationSendDirect');
  assert.equal(sends.length,2);assert.deepEqual(sends[0].params,sends[1].params);
  assert.equal(f.calls.some(c=>c.operation==='conversationSend'),false);f.panel.close();
});

test('agent list reports CLI version, account and model count without claiming model access', async () => {
  const f = await fixture({
    discover: () => ({ success: true, result: { agents: [{ id: 'codex', name: 'Codex', executable: '/x/codex' }, { id: 'claude', name: 'Claude Code', configFiles: ['/c'] }] } }),
    agentInspect: () => ({ success: true, result: { inspection: { versionReported: '0.50.0', account: { status: 'reportedSignedIn', kind: 'chatgpt' }, models: [{}, {}], complete: true } } }),
  });
  await settle();
  const lines = f.nodes.get('agentList').children.map(row => row.children[0].children[1].textContent);
  assert.deepEqual(lines, ['v0.50.0 · Signed in (chatgpt) · 2 models']);
  // Agents that are not installed move to the chips below the list.
  const missing = f.nodes.get('agentMissingList').children.map(row => row.children[0].children[1].textContent);
  assert.deepEqual(missing, ['Configuration found; program not installed']);
  assert.equal(f.nodes.get('agentMissing').hidden, false);
  assert.equal(f.nodes.get('agentMissingCount').textContent, '1');
  assert.equal(f.calls.filter(call => call.operation === 'agentInspect').length, 1);
  f.panel.close();
});

test('agents you can chat with move ahead of installed agents in the list', async () => {
  const f = await fixture({
    discover: () => ({ success: true, result: { agents: [{ id: 'gemini', name: 'Gemini CLI', executable: '/x/gemini' }, { id: 'codex', name: 'Codex', executable: '/x/codex' }] } }),
    agentInspect: () => ({ success: true, result: { inspection: { versionReported: '0.50.0', account: { status: 'reportedSignedIn', kind: 'chatgpt' }, models: [], complete: true } } }),
  });
  f.nodes.get('agentsTab').fire('click');
  for (let i = 0; i < 5; i++) await settle();
  const rows = f.nodes.get('agentList').children;
  assert.deepEqual(rows.map(row => row.children[0].children[0].textContent), ['Codex', 'Gemini CLI']);
  assert.match(rows[0].className, /\bready\b/);
  f.panel.close();
});

test('a failed explicit-model conversation offers a retry that starts a default-model conversation with the same prompt', async () => {
  const failed = { ...chat('a', 'failed'), agentId: 'codex', model: 'bad-model', requestedModel: 'bad-model' };
  const created = [];
  const f = await fixture({
    conversationList: () => ({ success: true, result: { conversations: [failed] } }),
    conversationRead: params => ({ success: true, result: { conversation: params.conversationId === 'a' ? failed : chat(params.conversationId), journal: params.conversationId === 'a' ? { events: [{ sequence: 1, kind: 'user', data: { messageId: 'm', text: 'ping' } }, { sequence: 2, kind: 'terminal', data: { status: 'failed', error: 'model not supported' } }], cursor: 2, more: false } : { events: [], cursor: 0, more: false } } }),
    conversationCreate: params => { created.push(params); return { success: true, result: { conversation: chat('new') } }; },
    conversationSend: params => ({ success: true, result: { conversation: chat('new', 'running'), echo: params } }),
  }, { ab_panel_conversation: 'a' });
  await settle();
  const retry = f.nodes.get('transcript').querySelectorAll('button').find(node => node.textContent === 'Retry with default model');
  assert.ok(retry); retry.fire('click'); await settle(); await settle();
  assert.deepEqual(created, [{ agentId: 'codex' }]);
  assert.equal(f.calls.find(call => call.operation === 'conversationSend').params.text, 'ping');
  f.panel.close();
});

test('a failure on the CLI-configured model offers its configuration instead of a retry that would repeat it', async () => {
  const failed = { ...chat('a', 'failed'), agentId: 'codex', model: 'configured-model', requestedModel: null };
  const f = await fixture({
    conversationList: () => ({ success: true, result: { conversations: [failed] } }),
    conversationRead: () => ({ success: true, result: { conversation: failed, journal: { events: [{ sequence: 1, kind: 'user', data: { messageId: 'm', text: 'ping' } }, { sequence: 2, kind: 'terminal', data: { status: 'failed', error: 'model not supported' } }], cursor: 2, more: false } } }),
    configRead: params => ({ success: true, result: { configuration: { agentId: params.agentId, path: '/c', fields: [] } } }),
  }, { ab_panel_conversation: 'a' });
  await settle();
  const labels = f.nodes.get('transcript').querySelectorAll('button').map(node => node.textContent);
  assert.deepEqual(labels, ['Change default model']);
  f.nodes.get('transcript').querySelectorAll('button')[0].fire('click'); await settle();
  assert.equal(f.nodes.get('agentsView').hidden, false);
  assert.ok(f.calls.some(call => call.operation === 'configRead' && call.params.agentId === 'codex'));
  f.panel.close();
});

test('deleting asks first, then forgets the conversation and its unsent draft', async () => {
  const support = {...fullSupport, conversationDelete: true};
  const f = await fixture({
    status: () => ({connected:true, agentPanel:support}),
    conversationDelete: params => ({ success: true, result: { deleted: { id: params.conversationId, cliHistory: { removed: [], failed: [] }, browserSession: { closed: true } } } }),
  }, { ab_panel_conversation: 'b' });
  await settle();
  assert.equal(f.nodes.get('deleteChat').hidden, false);
  f.nodes.get('message').value = 'unsent';
  f.nodes.get('deleteChat').fire('click');
  assert.equal(f.nodes.get('deleteConfirm').hidden, false);
  assert.ok(!f.calls.some(call => call.operation === 'conversationDelete'));
  f.chats.splice(1, 1);
  f.nodes.get('confirmDelete').fire('click'); await settle(); await settle();
  assert.deepEqual(f.calls.filter(call => call.operation === 'conversationDelete').map(call => call.params), [{ conversationId: 'b' }]);
  assert.equal(f.nodes.get('conversation').value, '');
  assert.equal(f.nodes.get('message').value, '');
  assert.equal(f.nodes.get('deleteConfirm').hidden, true);
  assert.equal(f.nodes.get('deleteChat').hidden, true);
  f.panel.close();
});

test('hosts that do not report deletion never show the delete control', async () => {
  const f = await fixture({}, { ab_panel_conversation: 'b' });
  await settle();
  assert.equal(f.nodes.get('deleteChat').hidden, true);
  f.panel.close();
});

test('new conversations default to full access, and the remembered choice is sent at creation', async () => {
  const support = {...fullSupport, fullAccess: true};
  const created = [];
  const overrides = {
    status: () => ({connected:true, agentPanel:support}),
    conversationCreate: params => { created.push(params); return { success: true, result: { conversation: chat('n') } }; },
    conversationSend: () => ({ success: true, result: { conversation: chat('n', 'running') } }),
  };
  const f = await fixture(overrides);
  assert.equal(f.nodes.get('access').hidden, false);
  assert.equal(f.nodes.get('accessLabel').textContent, 'Full access');
  f.nodes.get('message').value = 'go'; await f.panel.send(); await settle();
  assert.equal(created[0].fullAccess, true);
  f.panel.close();
  const g = await fixture(overrides, { ab_panel_full_access: false });
  assert.equal(g.nodes.get('accessLabel').textContent, 'Ask first');
  g.nodes.get('access').fire('click'); await settle();
  assert.equal(g.nodes.get('accessLabel').textContent, 'Full access');
  assert.ok(g.writes.some(w => w.ab_panel_full_access === true));
  g.panel.close();
});

test('an existing conversation switches access in place and approvals offer allow-all', async () => {
  const support = {...fullSupport, fullAccess: true};
  const waiting = { ...chat('a', 'waitingInput'), fullAccess: false };
  const changes = [];
  const f = await fixture({
    status: () => ({connected:true, agentPanel:support}),
    conversationList: () => ({ success: true, result: { conversations: [waiting] } }),
    conversationRead: () => ({ success: true, result: { conversation: waiting, journal: { events: [{ sequence: 1, kind: 'request', data: { requestId: 3, method: 'item/commandExecution/requestApproval', params: { command: 'ls' } } }], cursor: 1, more: false } } }),
    conversationSetAccess: params => { changes.push(params); waiting.fullAccess = params.fullAccess; return { success: true, result: { conversation: { ...waiting, fullAccess: params.fullAccess } } }; },
  }, { ab_panel_conversation: 'a' });
  await settle();
  assert.equal(f.nodes.get('accessLabel').textContent, 'Ask first');
  const allowAll = f.nodes.get('requests').querySelectorAll('button').find(node => node.textContent === 'Allow all in this conversation');
  assert.ok(allowAll); allowAll.fire('click'); await settle();
  assert.deepEqual(changes, [{ conversationId: 'a', fullAccess: true }]);
  assert.equal(f.nodes.get('accessLabel').textContent, 'Full access');
  f.panel.close();
});

test('hosts without full-access support hide the control and never send the option', async () => {
  const f = await fixture();
  assert.equal(f.nodes.get('access').hidden, true);
  f.panel.close();
});

test('the tab beside the panel is sent with the next message unless the user removes it', async () => {
  const support = {...fullSupport, pageContext: true};
  const sends = [];
  const tabs = { async query() { return [{ id: 7, url: 'https://x.com/post/1', title: 'A post' }]; } };
  const overrides = {
    tabs, status: () => ({connected:true, agentPanel:support}),
    conversationSend: params => { sends.push(params); return { success: true, result: { conversation: chat('b', 'running') } }; },
  };
  const f = await fixture(overrides, { ab_panel_conversation: 'b' });
  await settle();
  assert.equal(f.nodes.get('pageChip').hidden, false);
  assert.equal(f.nodes.get('pageTitle').textContent, 'A post');
  f.nodes.get('message').value = 'what did he reply to'; await f.panel.send(); await settle();
  assert.deepEqual(sends[0].page, { tabId: 7, url: 'https://x.com/post/1', title: 'A post' });
  f.nodes.get('pageDetach').fire('click');
  assert.equal(f.nodes.get('pageChip').hidden, true);
  f.nodes.get('message').value = 'second'; await f.panel.send(); await settle();
  assert.equal(sends[1].page, undefined);
  f.panel.close();
  const g = await fixture({ ...overrides, status: () => ({connected:true, agentPanel:fullSupport}) }, { ab_panel_conversation: 'b' });
  await settle();
  g.nodes.get('message').value = 'old host'; await g.panel.send(); await settle();
  assert.equal(sends[2].page, undefined);
  assert.equal(g.nodes.get('pageChip').hidden, true);
  g.panel.close();
});

test('OpenCode is selectable only when the confirmed host advertises its adapter', async () => {
  const f = await fixture({status:()=>({connected:true,agentPanel:{...fullSupport,chatAgents:['codex','claude','opencode']}})});
  assert.deepEqual(f.nodes.get('agent').children.map(option=>option.value),['codex','claude','opencode']);
  f.hostUpdate({connected:true,agentPanel:fullSupport});
  assert.deepEqual(f.nodes.get('agent').children.map(option=>option.value),['codex','claude']);
  f.panel.close();
});

test('ACP permissions show the command and return the exact selected option', async () => {
  const params={toolCall:{title:'Run a command',rawInput:{command:'ls'}},options:[{optionId:'once',kind:'allow_once',name:'Allow once'},{optionId:'deny',kind:'reject_once',name:'Reject'}]};
  const f=await fixture({conversationRead:async()=>({success:true,result:{conversation:chat('a','waitingInput'),journal:{events:[{sequence:1,kind:'request',data:{requestId:'permission',method:'session/request_permission',params}}],cursor:1,more:false}}}),conversationRespond:async()=>({success:true,result:{}})});
  f.panel.select('a');await settle();
  assert.match(f.nodes.get('requests').querySelectorAll('pre')[0].textContent,/ls/);
  const allow=f.nodes.get('requests').querySelectorAll('button').find(button=>button.textContent==='Allow once');
  allow.fire('click');await settle();
  assert.deepEqual(f.calls.find(call=>call.operation==='conversationRespond').params.result,{outcome:{outcome:'selected',optionId:'once'}});
  f.panel.close();
});

test('every controller node exists exactly once in the shipped sidepanel markup', async () => {
  const {readFile} = await import('node:fs/promises');
  const source=await readFile(new URL('./sidepanel.js',import.meta.url),'utf8');
  const markup=await readFile(new URL('./sidepanel.html',import.meta.url),'utf8');
  const ids=[...source.match(/const ids = \[([^\]]+)\]/)[1].matchAll(/'([^']+)'/g)].map(match=>match[1]);
  for (const id of ids) assert.equal([...markup.matchAll(/\bid="([^"]+)"/g)].filter(match=>match[1]===id).length,1,`Missing or duplicate controller node ${id}`);
});

test('OpenCode permission labels describe CLI rules without promising a prompt for every tool', async () => {
  const f=await fixture({status:()=>({connected:true,agentPanel:{...fullSupport,fullAccess:true,chatAgents:['codex','claude','opencode']}})});
  f.chats[1].agentId='opencode';f.chats[1].fullAccess=false;
  f.panel.select('b');await settle();
  assert.equal(f.nodes.get('accessLabel').textContent,'CLI permissions');
  assert.match(f.nodes.get('access').title,/may allow tools without asking/);
  f.panel.close();
});

test('Pi is offered when the host runs it, and without full access it is labelled browser only', async () => {
  const support = {...fullSupport, fullAccess: true, chatAgents: ['codex', 'claude', 'opencode', 'pi']};
  const f = await fixture({ status: () => ({connected:true, agentPanel:support}) }, { ab_panel_full_access: false });
  assert.deepEqual(f.nodes.get('agent').children.map(o => o.value), ['codex', 'claude', 'opencode', 'pi']);
  f.nodes.get('agent').value = 'pi'; f.nodes.get('agent').fire('change'); await settle();
  assert.equal(f.nodes.get('accessLabel').textContent, 'Browser only');
  f.nodes.get('agent').value = 'opencode'; f.nodes.get('agent').fire('change'); await settle();
  assert.equal(f.nodes.get('accessLabel').textContent, 'CLI permissions');
  f.panel.close();
});

test('Agents & models never runs more than two CLI probes at once', async () => {
  const support = {...fullSupport, chatAgents: ['codex', 'claude', 'opencode', 'pi']};
  let running = 0, peak = 0; const release = [];
  const f = await fixture({
    status: () => ({connected:true, agentPanel:support}),
    discover: () => ({ success: true, result: { agents: ['codex','claude','opencode','pi'].map(id => ({ id, name: id, executable: `/x/${id}` })) } }),
    agentInspect: params => { running++; peak = Math.max(peak, running); return new Promise(resolve => release.push(() => { running--; resolve({ success: true, result: { inspection: { agentId: params.agentId, versionReported: '1.0.0', account: { status: 'unverified' }, models: [], complete: true } } }); })); },
  });
  f.nodes.get('agentsTab').fire('click');
  for (let i = 0; i < 20 && (release.length || running); i++) { await settle(); release.shift()?.(); await settle(); }
  assert.ok(peak <= 2, `peak ${peak}`);
  const lines = f.nodes.get('agentList').children.map(row => row.children[0].children[1].textContent);
  assert.ok(lines.every(line => line.startsWith('v1.0.0')), lines.join(' | '));
  f.panel.close();
});

test('the conversation menu shows this site or everything, by main domain', async () => {
  const chats = [{ ...chat('x1'), title: 'reply on x', site: 'x.com', agentId: 'codex' }, { ...chat('x2'), title: 'mobile x', site: 'mobile.x.com', agentId: 'pi' }, { ...chat('gh'), title: 'repos', site: 'github.com', agentId: 'claude' }, { ...chat('old'), title: 'no site' }];
  const tabs = { async query() { return [{ id: 3, url: 'https://x.com/someone/status/1', title: 'post' }]; } };
  const f = await fixture({ tabs, status: () => ({connected:true, agentPanel:{...fullSupport, pageContext:true}}), conversationList: () => ({ success: true, result: { conversations: chats } }) });
  await settle();
  const titles = () => f.nodes.get('conversationItems').children.slice(1).map(item => item.children[0]?.children[0]?.textContent);
  f.nodes.get('conversationButton').fire('click');
  assert.equal(f.nodes.get('conversationMenu').hidden, false);
  assert.equal(f.nodes.get('scopeSite').textContent, 'This site · x.com');
  assert.deepEqual(titles(), ['reply on x', 'mobile x']);
  f.nodes.get('scopeAll').fire('click');
  assert.deepEqual(titles(), ['reply on x', 'mobile x', 'repos', 'no site']);
  assert.ok(f.writes.some(w => w.ab_panel_conversation_scope === 'all'));
  f.nodes.get('conversationItems').children[3].children[0].fire('click'); await settle();
  assert.equal(f.nodes.get('conversation').value, 'gh');
  assert.equal(f.nodes.get('conversationTitle').textContent, 'repos');
  assert.equal(f.nodes.get('conversationMenu').hidden, true);
  f.panel.close();
});

test('main domains group subdomains and keep two-level public suffixes', async () => {
  const { mainDomain } = await import('./sidepanel.js');
  assert.equal(mainDomain('mobile.x.com'), 'x.com');
  assert.equal(mainDomain('www.bbc.co.uk'), 'bbc.co.uk');
  assert.equal(mainDomain('news.sina.com.cn'), 'sina.com.cn');
  assert.equal(mainDomain('localhost'), 'localhost');
  assert.equal(mainDomain('127.0.0.1'), '127.0.0.1');
});

test('consecutive tool calls fold into one group that stays open only while a tool runs', async () => {
  const tool = (sequence, id, extra = {}, completed = true) => ({ sequence, kind: 'tool', data: { messageId: 'm', completed, item: { id, type: 'mcpToolCall', tool: 'chrome_use_open', arguments: { url: 'https://example.com' }, ...extra } } });
  const events = [{ sequence: 1, kind: 'user', data: { messageId: 'm', text: 'go' } }, tool(2, 't1'), tool(3, 't2', { error: 'boom' }), tool(4, 't3'), { sequence: 5, kind: 'textFinal', data: { messageId: 'm', itemId: 'a', part: 0, text: 'done' } }, tool(6, 't4'), tool(7, 't5', {}, false)];
  let read = { events, cursor: 7 };
  const f = await fixture({
    conversationRead: params => { const page = { events: read.events.filter(e => e.sequence > params.after), cursor: read.cursor, more: false }; return { success: true, result: { conversation: chat('a', read.cursor > 7 ? 'idle' : 'running'), journal: page } }; },
  }, { ab_panel_conversation: 'a' });
  await settle();
  const groups = () => f.nodes.get('transcript').querySelectorAll('details').filter(node => node.className === 'tool-group');
  assert.equal(groups().length, 2);
  assert.equal(groups()[0].children[0].textContent, '3 steps · 1 failed');
  assert.equal(groups()[0].open, false);
  assert.equal(groups()[1].children[0].textContent, 'Working · 2 steps');
  assert.equal(groups()[1].open, true);
  // The same group folds once its last step completes on a later read.
  read = { events: [...events, tool(8, 't5')], cursor: 8 };
  await f.panel.refresh(); await settle();
  assert.equal(groups().length, 2);
  assert.equal(groups()[1].children[0].textContent, '2 steps');
  assert.equal(groups()[1].open, false);
  f.panel.close();
});
test('conversation options persist per chat without writing the agent global configuration', async () => {
  const value={...chat('b'),agentId:'codex',options:{effort:'high',serviceTier:'default',toolAccess:'readFiles'}};
  const f=await fixture({status:()=>({connected:true,agentPanel:{...fullSupport,conversationOptions:true}}),conversationList:async()=>({success:true,result:{conversations:[value]}}),conversationRead:async()=>({success:true,result:{conversation:value,journal:{events:[],cursor:0}}}),conversationSetOptions:async params=>({success:true,result:{conversation:{...value,options:params.options}}})});
  f.panel.select('b');await settle();f.nodes.get('turnEffort').value='low';f.nodes.get('turnTier').value='priority';f.nodes.get('turnAccess').value='writeFiles';f.nodes.get('saveTurnOptions').fire('click');await settle();
  const call=f.calls.find(c=>c.operation==='conversationSetOptions');assert.deepEqual(call.params,{conversationId:'b',options:{effort:'low',serviceTier:'priority',toolAccess:'writeFiles'}});
  assert.equal(f.calls.some(c=>['configApply','configPlan'].includes(c.operation)),false);
  f.panel.close();
});

test('explicit skill selection snapshots its input before sending and retains the token on retry', async () => {
  let sends=0;
  const skill={id:'catalog-id',name:'example',source:'library',description:'Example',enabled:true,editable:true};
  const f=await fixture({status:()=>({connected:true,agentPanel:{...fullSupport,skillLibrary:true}}),skillList:async()=>({success:true,result:{skills:[skill]}}),conversationSkillSelect:async()=>({success:true,result:{skill:{id:'snapshot-token',name:'example'}}}),conversationSend:async()=>{sends++;throw new Error('lost acknowledgement');},conversationRead:async()=>({success:true,result:{conversation:{...chat('b'),agentId:'codex'},journal:{events:[],cursor:0}}})});
  f.panel.select('b');await settle();f.nodes.get('message').value='/example task';f.nodes.get('composer').fire('submit');await settle();await settle();
  assert.equal(f.calls.find(c=>c.operation==='conversationSend').params.skillId,'snapshot-token');
  await f.panel.refresh();await settle();f.nodes.get('retryPending').fire('click');await settle();
  assert.equal(sends,2);assert.equal(f.calls.filter(c=>c.operation==='conversationSkillSelect').length,1);
  assert.equal(f.calls.filter(c=>c.operation==='conversationSend')[1].params.skillId,'snapshot-token');f.panel.close();
});

test('image uploads complete before delivery and explicit retry preserves the same images and page', async () => {
  const attachment={id:'a'.repeat(64),mediaType:'image/png',bytes:12};
  const f=await fixture({status:()=>({connected:true,agentPanel:{...fullSupport,imageAttachments:true,pageContext:true}}),encodeImage:async()=>({name:'fixture.png',mediaType:'image/png',data:'synthetic-base64'}),conversationAttachmentPut:async()=>({success:true,result:{attachment}}),conversationSend:async()=>{throw new Error('lost acknowledgement');},tabs:{async query(){return [{id:42,title:'Fixture',url:'https://example.test'}];}}});
  f.panel.select('b');await settle();f.nodes.get('imageFiles').files=[{type:'image/png'}];f.nodes.get('imageFiles').fire('change');await settle();
  f.nodes.get('message').value='Check this image';f.nodes.get('composer').fire('submit');await settle();await settle();
  const first=f.calls.find(c=>c.operation==='conversationSend');assert.deepEqual(first.params.attachments,[attachment]);assert.equal(first.params.page.tabId,42);
  assert.ok(f.calls.findIndex(c=>c.operation==='conversationAttachmentPut')<f.calls.findIndex(c=>c.operation==='conversationSend'));
  await f.panel.refresh();f.nodes.get('retryPending').fire('click');await settle();
  assert.deepEqual(f.calls.filter(c=>c.operation==='conversationSend')[1].params,first.params);
  assert.equal(f.calls.filter(c=>c.operation==='conversationAttachmentPut').length,1);f.panel.close();
});

test('HEIC files reach the host converter only when the CLI advertises it', async () => {
  for (const heicConvert of [true, false]) {
    const seen=[];
    const f=await fixture({status:()=>({connected:true,agentPanel:{...fullSupport,imageAttachments:true,...(heicConvert?{heicConvert}:{})}}),encodeImage:async(file,{convertHeif})=>{seen.push(convertHeif);if(!convertHeif)throw new Error('Update the chrome-use CLI to attach HEIC photos, or export them as JPEG first.');await convertHeif(file);return {name:file.name,mediaType:'image/jpeg',data:'converted'};},imageConvert:async()=>({success:true,result:{schemaVersion:1,mediaType:'image/jpeg',data:btoa('JPEG')}})});
    f.panel.select('b');await settle();
    const file={name:'IMG_0001.HEIC',type:'',size:4,async arrayBuffer(){return new Uint8Array([1,2,3,4]).buffer;}};
    f.nodes.get('message').fire('paste',{clipboardData:{files:[file]}});await settle();await settle();
    assert.equal(seen.length,1,'an untyped .heic paste is not dropped');
    assert.equal(typeof seen[0]==='function',heicConvert);
    assert.equal(f.calls.filter(c=>c.operation==='imageConvert').length,heicConvert?1:0);
    if(!heicConvert)assert.match(f.nodes.get('notice').textContent,/Update the chrome-use CLI/);
    f.panel.close();
  }
});

test('a failed image upload never sends the text alone or creates an uncertain message', async () => {
  const f=await fixture({status:()=>({connected:true,agentPanel:{...fullSupport,imageAttachments:true}}),encodeImage:async()=>({name:'fixture.png',mediaType:'image/png',data:'synthetic'}),conversationAttachmentPut:async()=>{throw new Error('image save failed');}});
  f.panel.select('b');await settle();f.nodes.get('imageFiles').files=[{type:'image/png'}];f.nodes.get('imageFiles').fire('change');await settle();
  f.nodes.get('message').value='Keep image';f.nodes.get('composer').fire('submit');await settle();await settle();
  assert.equal(f.calls.some(c=>c.operation==='conversationSend'),false);assert.equal(f.nodes.get('send').disabled,false);assert.equal(f.nodes.get('message').value,'Keep image');assert.equal(f.nodes.get('attachmentDrafts').hidden,false);f.panel.close();
});

test('opening the panel never starts a CLI the user has not picked', async () => {
  const installed=[{id:'codex',name:'Codex',executable:'/usr/local/bin/codex'},{id:'claude',name:'Claude Code',executable:'/usr/local/bin/claude'}];
  const f=await fixture({hiddenNodes:['agentsView'],discover:()=>({success:true,result:{agents:installed}}),agentInspect:()=>{throw new Error('probe started');}});
  assert.equal(f.calls.filter(call=>call.operation==='agentInspect').length,0);
  assert.match(f.nodes.get('modelStatus').textContent,/not been checked/);f.panel.close();
});

test('the saved agent is restored and only its CLI is read', async () => {
  const f=await fixture({agentInspect:params=>({success:true,result:{inspection:{agentId:params.agentId,models:[],account:{status:'unverified'},complete:true}}})},{ab_panel_agent:'claude'});
  assert.equal(f.nodes.get('agent').value,'claude');
  assert.deepEqual(f.calls.filter(call=>call.operation==='agentInspect').map(call=>call.params.agentId),['claude']);
  f.nodes.get('agent').value='codex';f.nodes.get('agent').fire('change');await settle();
  assert.ok(f.writes.some(write=>write.ab_panel_agent==='codex'));f.panel.close();
});

test('without a saved choice the agent of the latest conversation is used', async () => {
  const f=await fixture({conversationList:()=>({success:true,result:{conversations:[{...chat('old'),agentId:'codex',updated:1},{...chat('new'),agentId:'claude',updated:2}]}}),
    agentInspect:params=>({success:true,result:{inspection:{agentId:params.agentId,models:[],account:{status:'unverified'},complete:true}}})});
  assert.equal(f.nodes.get('agent').value,'claude');
  assert.deepEqual(f.calls.filter(call=>call.operation==='agentInspect').map(call=>call.params.agentId),['claude']);f.panel.close();
});

test('a failed CLI read says why', async () => {
  const f=await fixture({agentInspect:()=>({success:false,error:'The system stopped this CLI as soon as it started'})},{ab_panel_agent:'codex'});
  assert.match(f.nodes.get('modelStatus').textContent,/stopped this CLI as soon as it started/);f.panel.close();
});

test('composer keeps stop and send together in one right-aligned group', async () => {
  const { readFileSync } = await import('node:fs');
  const html = readFileSync(new URL('./sidepanel.html', import.meta.url), 'utf8');
  const group = /<span class="composer-send">([\s\S]*?)<\/span>/.exec(html)?.[1] || '';
  for (const id of ['resume', 'sendDirect', 'stop', 'send']) assert.match(group, new RegExp(`id="${id}"`), `${id} is in the send group`);
  const css = readFileSync(new URL('./sidepanel.css', import.meta.url), 'utf8');
  assert.match(css, /\.composer-send \{[^}]*margin-left:auto/);
  assert.match(css, /\.composer-send \{[^}]*flex:none/);
});

test('hosts with request revisions get a decision bound to the card the user saw', async () => {
  const support = {...fullSupport, requestRevision: 1};
  const params = {tool_name:'mcp__chrome_use__chrome_use_fill', input:{selector:'#password', value:'[redacted input, 8 chars]'}};
  const reads = [
    [{sequence:1,kind:'request',data:{requestId:'r',method:'can_use_tool',params,generation:3,requestRevision:7}}],
    [{sequence:2,kind:'request',data:{requestId:'r',method:'can_use_tool',params:{...params,input:{selector:'#other',value:'x'}},generation:3,requestRevision:8}}],
  ];
  const f = await fixture({
    status: () => ({connected:true, agentPanel:support}),
    conversationRead: async ({after}) => { const events = reads[after] || []; return {success:true,result:{conversation:chat('a','waitingInput'),journal:{events,cursor:after+events.length,more:false}}}; },
    conversationRespond: async () => ({success:true,result:{response:{dispatched:true,confirmed:false}}}),
  });
  f.hostUpdate({connected:true, agentPanel:support}); await settle();
  f.panel.select('a'); await settle();
  const first = f.nodes.get('requests').querySelectorAll('button').find(b => b.textContent === 'Allow once');
  await f.panel.refresh(); await settle();
  // The re-sent request replaced the card; the old card is gone.
  const cards = f.nodes.get('requests').querySelectorAll('pre');
  assert.equal(cards.length, 1); assert.match(cards[0].textContent, /#other/);
  assert.equal(f.nodes.get('requests').querySelectorAll('button').includes(first), false);
  f.nodes.get('requests').querySelectorAll('button').find(b => b.textContent === 'Allow once').fire('click'); await settle();
  const sent = f.calls.find(call => call.operation === 'conversationRespond').params;
  assert.deepEqual(sent, {conversationId:'a', requestId:'r', generation:3, requestRevision:8, decision:'allow'});
  assert.equal(JSON.stringify(sent).includes('redacted'), false);
  f.panel.close();
});

test('question answers are sent as validated decisions, never as echoed input', async () => {
  const support = {...fullSupport, requestRevision: 1};
  const input = {questions:[{question:'Which size?',multiSelect:false,options:[{label:'Small'},{label:'Large'}]},{question:'Extras?',multiSelect:true,options:[{label:'Bag'},{label:'Card'}]}], note:'[redacted secret, 9 chars]'};
  const codex = {questions:[{id:'q1',question:'Pick',isOther:false,options:[{label:'A'}]},{id:'q2',question:'Name',isOther:false,options:null}]};
  const f = await fixture({
    status: () => ({connected:true, agentPanel:support}),
    conversationRead: async () => ({success:true,result:{conversation:chat('a','waitingInput'),journal:{events:[
      {sequence:1,kind:'request',data:{requestId:'q',method:'can_use_tool',params:{tool_name:'AskUserQuestion',input},generation:1,requestRevision:2}},
      {sequence:2,kind:'request',data:{requestId:9,method:'item/tool/requestUserInput',params:codex,generation:1,requestRevision:3}},
    ],cursor:2,more:false}}}),
    conversationRespond: async () => ({success:true,result:{response:{dispatched:true,confirmed:false}}}),
  });
  f.hostUpdate({connected:true, agentPanel:support}); await settle();
  f.panel.select('a'); await settle();
  const [claude, codexCard] = f.nodes.get('requests').children;
  const inputs = claude.querySelectorAll('input');
  inputs.find(i => i.value === 'Large').checked = true;
  inputs.find(i => i.value === 'Bag').checked = true; inputs.find(i => i.value === 'Card').checked = true;
  claude.querySelectorAll('button').find(b => b.textContent === 'Send answers').fire('click'); await settle();
  const answered = f.calls.filter(call => call.operation === 'conversationRespond').map(call => call.params);
  assert.deepEqual(answered[0], {conversationId:'a', requestId:'q', generation:1, requestRevision:2, decision:{answersByPosition:[{option:1}, {options:[0,1]}]}});
  // Codex: free text only where the question allows it (q2 has no options).
  const codexInputs = codexCard.querySelectorAll('input');
  assert.equal(codexInputs.filter(i => i.placeholder === 'Your answer').length, 1);
  codexInputs.find(i => i.value === 'A').checked = true;
  codexInputs.find(i => i.placeholder === 'Your answer').value = 'Ada';
  codexCard.querySelectorAll('button').find(b => b.textContent === 'Send answers').fire('click'); await settle();
  assert.deepEqual(f.calls.filter(call => call.operation === 'conversationRespond')[1].params.decision, {answersByPosition:[{option:0}, {custom:'Ada'}]});
  f.panel.close();
});

test('an older host without request revisions still gets the legacy result shape', async () => {
  const params = {tool_name:'Bash', input:{command:'ls'}};
  const f = await fixture({
    conversationRead: async () => ({success:true,result:{conversation:chat('a','waitingInput'),journal:{events:[{sequence:1,kind:'request',data:{requestId:'r',method:'can_use_tool',params}}],cursor:1,more:false}}}),
    conversationRespond: async () => ({success:true,result:{}}),
  });
  f.panel.select('a'); await settle();
  f.nodes.get('requests').querySelectorAll('button').find(b => b.textContent === 'Allow once').fire('click'); await settle();
  assert.deepEqual(f.calls.find(call => call.operation === 'conversationRespond').params, {conversationId:'a', requestId:'r', result:{behavior:'allow', updatedInput:{command:'ls'}}});
  f.panel.close();
});

test('the panel reports the conversation it shows and switches when a notification is clicked', async () => {
  const f = await fixture({ presence: true }, { ab_panel_conversation: 'a' });
  assert.equal(f.presence.name, 'ab-panel-presence');
  assert.deepEqual(f.presence.posted.at(-1), { type: 'presence', windowId: 7, conversationId: 'a', visible: true });
  f.presence.listeners.message({ type: 'select', conversationId: 'b' }); await settle();
  assert.equal(f.nodes.get('conversation').value, 'b');
  assert.deepEqual(f.presence.posted.at(-1), { type: 'presence', windowId: 7, conversationId: 'b', visible: true });
  f.nodes.get('agentsTab').fire('click'); await settle();
  assert.equal(f.presence.posted.at(-1).visible, false);
  f.panel.close();
});

test('agent replies to Inspect & Ask batches are listed and reported as seen once shown', async () => {
  const replies = [{ deliveryId: 'd', channel: 'dm-a', seq: 4, from: 'chrome-use-91', text: 'Fixed **it**', receivedAt: 1 }];
  const f = await fixture({ presence: true, status: () => ({ connected: true, agentPanel: { ...fullSupport, inspectReplies: 1 } }), inspectReplies: async () => ({ success: true, result: { replies } }) }, { ab_panel_conversation: 'a' });
  await settle();
  const box = f.nodes.get('inspectReplies');
  assert.equal(box.hidden, false);
  assert.match(f.nodes.get('inspectReplyList').children[0].children[0].children[0].textContent, /Reply from chrome-use-91/);
  assert.equal(f.presence.posted.at(-1).repliesShown, undefined, 'collapsed replies are not seen');
  f.presence.listeners.message({ type: 'showReplies' }); await settle();
  assert.equal(box.open, true);
  assert.equal(f.presence.posted.at(-1).repliesShown, true);
  f.panel.close();
});

test('a host without inspectReplies is never asked for replies', async () => {
  const f = await fixture({ presence: true });
  assert.equal(f.calls.some(call => call.operation === 'inspectReplies'), false);
  assert.equal(f.nodes.get('inspectReplies').hidden, true);
  f.panel.close();
});

// ---- /resume ----------------------------------------------------------------
const walk = (node, predicate) => [...(predicate(node) ? [node] : []), ...node.children.flatMap(child => walk(child, predicate))];
const textOf = node => [node.textContent, ...node.children.map(textOf)].filter(Boolean).join(' ');
const session = (id, extra = {}) => ({ sessionId: id, agentId: 'codex', title: `Session ${id}`, cwd: '/work/project', lastActive: Date.now() - 120000, running: false, resumable: true, conversationId: null, ...extra });
const resumeSupport = () => ({ connected: true, agentPanel: { ...fullSupport, sessionResume: 1, chatAgents: ['codex', 'claude'] } });

test('/resume is a built-in command: it opens the session picker instead of a skill lookup', async () => {
  const lists = [];
  const f = await fixture({ status: resumeSupport, conversationSessionList: params => { lists.push(params); return { success: true, result: { schemaVersion: 1, agentId: params.agentId, supported: true, sessions: [session('s1'), session('s2', { running: true })] } }; } });
  f.panel.select('b'); await settle();
  f.nodes.get('message').value = '/resume login bug'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  assert.equal(lists.length, 1);
  assert.equal(lists[0].query, 'login bug');
  assert.equal(lists[0].limit, 100);
  assert.ok(!f.calls.some(c => ['conversationSkillSelect', 'conversationSend', 'conversationCreate'].includes(c.operation)));
  assert.equal(f.nodes.get('message').value, '', 'the command is consumed');
  const host = f.nodes.get('resumeHost');
  const rows = walk(host, n => typeof n.className === 'string' && n.className.split(' ').includes('resume-item'));
  assert.equal(rows.length, 2);
  assert.match(textOf(rows[0]), /Session s1/);
  assert.match(textOf(rows[1]), /Active elsewhere/);
  f.panel.close();
});

test('an older CLI without session resume says to update it', async () => {
  const f = await fixture({});
  f.nodes.get('message').value = '/resume'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  assert.match(f.nodes.get('notice').textContent, /Update the chrome-use CLI to resume sessions/);
  assert.ok(!f.calls.some(c => c.operation === 'conversationSessionList'));
  f.panel.close();
});

test('choosing a session resumes it and selects the new conversation; an active one needs confirmation', async () => {
  const resumes = [], created = { ...chat('resumed'), agentId: 'codex', resumed: { sessionId: 's2', cwd: '/work/project' } };
  const f = await fixture({
    status: resumeSupport,
    conversationSessionList: params => ({ success: true, result: { schemaVersion: 1, agentId: params.agentId, supported: true, sessions: [session('s2', { running: true })] } }),
    conversationResume: params => { resumes.push(params); return { success: true, result: params.confirmRunning ? { schemaVersion: 1, conversation: created } : { schemaVersion: 1, confirmRequired: 'running', session: session('s2', { running: true }) } }; },
  });
  f.chats.push(created);
  f.nodes.get('message').value = '/resume'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  const host = f.nodes.get('resumeHost');
  walk(host, n => typeof n.className === 'string' && n.className.split(' ').includes('resume-item'))[0].fire('click'); await settle(); await settle();
  assert.deepEqual(resumes, [{ agentId: 'codex', sessionId: 's2' }]);
  assert.match(textOf(host), /still active in another terminal; continuing here can corrupt its history/);
  assert.notEqual(f.nodes.get('conversation').value, 'resumed', 'nothing is attached before the user confirms');
  walk(host, n => n.tag === 'button' && n.textContent === 'Continue here anyway')[0].fire('click'); await settle(); await settle(); await settle();
  assert.deepEqual(resumes[1], { agentId: 'codex', sessionId: 's2', confirmRunning: true });
  assert.equal(f.nodes.get('conversation').value, 'resumed');
  assert.ok(f.calls.some(c => c.operation === 'conversationRead' && c.params.conversationId === 'resumed'));
  f.panel.close();
});

test('the conversation menu offers to resume a session only when the CLI can', async () => {
  for (const [status, shown] of [[resumeSupport, true], [undefined, false]]) {
    const f = await fixture(status ? { status } : {});
    f.nodes.get('conversationButton').fire('click');
    const items = walk(f.nodes.get('conversationItems'), n => n.tag === 'button').map(textOf);
    assert.equal(items.some(t => /Resume a session…/.test(t)), shown);
    f.panel.close();
  }
});

// ---- project folder ------------------------------------------------------------
test('a new conversation is created in the chosen project folder, shown in the header', async () => {
  const created = [];
  const made = { ...chat('p'), agentId: 'codex', project: '/work/app' };
  const f = await fixture({
    tabs: { async query() { return [{ id: 5, url: 'http://localhost:3000/dash', title: 'Dash' }]; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } },
    status: () => ({ connected: true, agentPanel: { ...fullSupport, projectDirectory: 1, inspect: true, chatAgents: ['codex', 'claude'] } }),
    inspectProject: () => ({ success: true, result: { schemaVersion: 1, binding: null } }),
    conversationProjectList: () => ({ success: true, result: { schemaVersion: 1, projects: [{ path: '/work/app', sources: ['claude'] }] } }),
    conversationCreate: params => { created.push(params); return { success: true, result: { conversation: made } }; },
    conversationSend: () => ({ success: true, result: { conversation: made, message: { id: 'x', accepted: true, replayed: false } } }),
  });
  f.chats.push(made);
  f.panel.select(''); await settle(); await settle();
  assert.equal(f.nodes.get('projectChip').hidden, false);
  const select = f.nodes.get('project');
  select.value = '/work/app'; select.fire('change');
  f.nodes.get('message').value = 'Fix the build'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  assert.equal(created[0].project, '/work/app');
  assert.equal(f.nodes.get('projectPath').hidden, false);
  assert.match(f.nodes.get('projectPath').textContent, /Project: app/);
  assert.equal(f.nodes.get('projectPath').title, '/work/app');
  // The remember question stays up after creation (a reset must not hide it).
  const host = f.nodes.get('projectHost');
  assert.equal(host.children.find(c => c.className === 'project-remember').hidden, false);
  f.panel.close();
});

test('an older CLI without project folders is never sent one', async () => {
  const created = [];
  const f = await fixture({ conversationCreate: params => { created.push(params); return { success: true, result: { conversation: chat('n') } }; }, conversationSend: () => ({ success: true, result: { conversation: chat('n'), message: { id: 'x', accepted: true, replayed: false } } }) });
  f.panel.select(''); await settle();
  assert.equal(f.nodes.get('projectChip').hidden, true);
  assert.ok(!f.calls.some(c => c.operation === 'conversationProjectList'));
  f.nodes.get('message').value = 'Hello'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  assert.equal(created[0].project, undefined);
  f.panel.close();
});

test('the host\'s default conversation title is shown in the panel\'s language', async () => {
  const { titleOf } = await import('./sidepanel.js');
  const { installI18n } = await import('./test-i18n.js');
  installI18n('zh_CN');
  try {
    assert.equal(titleOf({ title: 'New conversation' }, 'panel_new_conversation'), '新会话');
    assert.equal(titleOf({ title: '' }, 'panel_new_conversation'), '新会话');
    assert.equal(titleOf({ title: 'Fix the build' }, 'panel_new_conversation'), 'Fix the build');
  } finally { installI18n('en'); }
});

test('a resumed session whose folder cannot be a project says so', async () => {
  const created = { ...chat('resumed'), agentId: 'codex', resumed: { sessionId: 's9', cwd: '/' } };
  const f = await fixture({
    status: () => ({ connected: true, agentPanel: { ...fullSupport, sessionResume: 1, chatAgents: ['codex'] } }),
    conversationSessionList: params => ({ success: true, result: { schemaVersion: 1, agentId: params.agentId, supported: true, sessions: [{ sessionId: 's9', agentId: 'codex', title: 'x', cwd: '/', lastActive: 0, running: false, resumable: true, conversationId: null }] } }),
    conversationResume: () => ({ success: true, result: { schemaVersion: 1, conversation: created, projectRefused: '/' } }),
  });
  f.chats.push(created);
  f.nodes.get('message').value = '/resume'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  const row = walk(f.nodes.get('resumeHost'), n => typeof n.className === 'string' && n.className.split(' ').includes('resume-item'))[0];
  row.fire('click'); await settle(); await settle(); await settle();
  assert.equal(f.nodes.get('conversation').value, 'resumed');
  assert.match(f.nodes.get('notice').textContent, /folder \/ is a system or home folder/);
  f.panel.close();
});

// ---- Tab modes (0.5.62) ------------------------------------------------------

const webTabs = () => [
  { id: 11, windowId: 7, url: 'https://old.example/a', title: 'Old', lastAccessed: 100 },
  { id: 12, windowId: 7, url: 'https://shop.example/cart', title: 'Cart', lastAccessed: 300 },
  { id: 13, windowId: 7, url: 'chrome://settings', title: 'Settings', lastAccessed: 400 },
  { id: 99, windowId: 7, url: 'chrome-extension://own/sidepanel.html?mode=tab', title: 'Panel', lastAccessed: 500 },
];
function tabApi(list = webTabs()) {
  const listeners = {};
  return { listeners, created: [], async query(filter) { return filter.windowId === 7 ? list : []; }, async getCurrent() { return { id: 99 }; }, async create(options) { this.created.push(options); return {}; },
    onActivated: { addListener(f) { listeners.activated = f; } }, onUpdated: { addListener(f) { listeners.updated = f; } }, onRemoved: { addListener(f) { listeners.removed = f; } } };
}
const grantSupport = { connected: true, agentPanel: { ...fullSupport, pageContext: true, tabGrant: 1 } };

test('a panel tab opens its own conversation and never changes the side panel\'s', async () => {
  const tabs = tabApi();
  const f = await fixture({ mode: 'tab', initialConversation: 'b', tabs, status: () => grantSupport }, { ab_panel_conversation: 'a' });
  await settle();
  assert.equal(f.nodes.get('conversation').value, 'b');
  f.panel.select('a'); await settle();
  assert.ok(!f.writes.some(w => 'ab_panel_conversation' in w), 'the side panel selection is untouched');
  assert.deepEqual(f.locations.slice(-1), ['a']);
  // "This page": the last web tab of the window, not the panel tab itself
  // and not a Chrome page; the user can pick another.
  assert.equal(f.nodes.get('pagePicker').value, '12');
  assert.equal(f.nodes.get('pagePicker').hidden, false);
  assert.equal(f.nodes.get('pageTitle').hidden, true);
  assert.deepEqual(f.nodes.get('pagePicker').children.map(o => o.value), ['11', '12']);
  f.nodes.get('pagePicker').value = '11'; f.nodes.get('pagePicker').fire('change'); await settle();
  assert.equal(f.nodes.get('pageTitle').textContent, 'Old');
  // A tab of the window becoming active does not override an explicit pick.
  tabs.listeners.activated({ tabId: 12, windowId: 7 }); await settle();
  assert.equal(f.nodes.get('pageTitle').textContent, 'Old');
  assert.equal(f.nodes.get('openInTab').hidden, true);
  f.panel.close();
});

test('the side panel opens the selected conversation in a tab', async () => {
  const tabs = tabApi();
  const f = await fixture({ tabs, getURL: path => `chrome-extension://own/${path}`, status: () => grantSupport }, { ab_panel_conversation: 'b' });
  assert.equal(f.nodes.get('openInTab').hidden, false);
  f.nodes.get('openInTab').fire('click'); await settle();
  assert.deepEqual(tabs.created, [{ url: 'chrome-extension://own/sidepanel.html?mode=tab&conversation=b' }]);
  f.panel.close();
});

test('a pending send from one view survives the other view saving its own', async () => {
  const f = await fixture({ conversationMessageStatus: async () => ({ success: true, result: { message: { known: true } } }) }, { ab_panel_conversation: 'a', ab_panel_pending: { a: 'accepted', other: 'kept' } });
  await settle();
  const pending = f.writes.filter(write => 'ab_panel_pending' in write);
  assert.ok(pending.length > 0);
  for (const write of pending) assert.equal(write.ab_panel_pending.other, 'kept');
  assert.ok(pending.some(write => !('a' in write.ab_panel_pending)));
  f.panel.close();
});

test('lending the page beside the panel, sending with it and taking it back', async () => {
  const grantCalls = [];
  const grant = { tabId: 12, conversationId: 'b', url: 'https://shop.example/cart', title: 'Cart' };
  const tabs = { async query() { return [{ id: 12, url: 'https://shop.example/cart', title: 'Cart' }]; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const f = await fixture({ tabs, status: () => grantSupport,
    tabGrant: request => { grantCalls.push(request); return request.action === 'list' ? { success: true, grants: [] } : request.action === 'grant' ? { success: true, grant, service: { granted: true, tabId: 12, adopted: true } } : { success: true, grant, service: { granted: false } }; },
    conversationSend: async params => ({ success: true, result: { conversation: chat('b'), message: { id: params.messageId, accepted: true, replayed: false } } }),
  }, { ab_panel_conversation: 'b' });
  await settle();
  const button = f.nodes.get('grantTab');
  assert.equal(button.hidden, false);
  assert.equal(f.nodes.get('grantBar').hidden, true);
  button.fire('click'); await settle(); await settle();
  assert.deepEqual(grantCalls.find(c => c.action === 'grant'), { type: 'ab-tab-grant', action: 'grant', tabId: 12, conversationId: 'b', conversationTitle: 'b' });
  assert.equal(f.nodes.get('grantBar').hidden, false);
  assert.match(f.nodes.get('grantText').textContent, /Cart/);
  assert.equal(button.hidden, true, 'the lent tab is not offered again');
  // Every message repeats the grant to the service.
  f.nodes.get('message').value = 'go'; await f.panel.send(); await settle();
  const sent = f.calls.find(c => c.operation === 'conversationSend');
  assert.deepEqual(sent.params.tabGrant, { tabId: 12, url: 'https://shop.example/cart', title: 'Cart' });
  f.nodes.get('grantRevoke').fire('click'); await settle(); await settle();
  assert.deepEqual(grantCalls.at(-1), { type: 'ab-tab-grant', action: 'revoke', conversationId: 'b' });
  assert.equal(f.nodes.get('grantBar').hidden, true);
  f.panel.close();
});

test('every view follows grants made elsewhere, and Stop leaves the tab lent', async () => {
  const grantCalls = [];
  const f = await fixture({ status: () => grantSupport, tabGrant: request => { grantCalls.push(request); return { success: true, grants: [], grant: null }; },
    conversationStop: async () => ({ success: true, result: { conversation: chat('a', 'stopping') } }) }, { ab_panel_conversation: 'a' });
  await settle();
  f.grantUpdate([{ tabId: 5, conversationId: 'a', url: 'chrome://newtab/', title: 'New Tab' }]);
  assert.equal(f.nodes.get('grantBar').hidden, false);
  assert.equal(f.nodes.get('grantNote').hidden, false, 'a page the agent cannot use is named');
  f.nodes.get('stop').fire('click'); await settle(); await settle();
  // Stopping a turn is not taking the tab back ("Take back the tab" is).
  assert.ok(!grantCalls.some(c => c.action === 'revoke'));
  f.grantUpdate([]);
  assert.equal(f.nodes.get('grantBar').hidden, true);
  f.panel.close();
});

test('a refused grant is explained in the panel\'s language', async () => {
  const tabs = { async query() { return [{ id: 12, url: 'https://shop.example/', title: 'Shop' }]; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const f = await fixture({ tabs, status: () => grantSupport, tabGrant: request => request.action === 'grant' ? { success: false, error: 'Another conversation is using this tab', reason: 'otherConversation' } : { success: true, grants: [] } }, { ab_panel_conversation: 'b' });
  await settle();
  f.nodes.get('grantTab').fire('click'); await settle(); await settle();
  assert.equal(f.nodes.get('notice').textContent, 'Another conversation is using this tab. Take it back there first.');
  assert.equal(f.nodes.get('grantBar').hidden, true);
  f.panel.close();
});

test('a title given in another view shows in this one', async () => {
  const f = await fixture({ conversationRead: async params => ({ success: true, result: { conversation: { ...chat(params.conversationId), title: 'Renamed elsewhere' }, journal: { events: [], cursor: 0, more: false } } }) }, { ab_panel_conversation: 'b' });
  await settle();
  assert.equal(f.nodes.get('conversationTitle').textContent, 'Renamed elsewhere');
  f.panel.close();
});

// ---- Slash completion anywhere in the text (0.5.63) ----------------------------

test('the `/word` at the caret is found after whitespace, not inside other words', () => {
  assert.deepEqual(slashQuery('/prof'), { query: 'prof', start: 0, end: 5 });
  assert.deepEqual(slashQuery('所有信息在 /profile'), { query: 'profile', start: 6, end: 14 });
  assert.deepEqual(slashQuery('use /'), { query: '', start: 4, end: 5 });
  assert.equal(slashQuery('see a/b'), null);
  assert.equal(slashQuery('https://x.example/pa'), null);
  assert.equal(slashQuery('/prof then more'), null, 'the caret is past the word');
  assert.deepEqual(slashQuery('/prof then more', 3), { query: 'pr', start: 0, end: 3 });
});

test('a skill completes in the middle of a message, by keyboard, and is used when sent', async () => {
  const skills = [
    { id: 'p', name: 'profile-use', source: 'user', description: 'Profile', enabled: true },
    { id: 'm', name: 'mail-use', source: 'user', description: 'Mail', enabled: true },
  ];
  const f = await fixture({ status: () => ({ connected: true, agentPanel: { ...fullSupport, skillLibrary: true } }), skillList: async () => ({ success: true, result: { skills } }),
    conversationSkillSelect: async params => ({ success: true, result: { skill: { id: `snap-${params.skillId}`, name: 'x' } } }),
    conversationSend: async params => ({ success: true, result: { conversation: chat('b'), message: { id: params.messageId, accepted: true, replayed: false } } }) }, { ab_panel_conversation: 'b' });
  await settle();
  const message = f.nodes.get('message'), picker = f.nodes.get('skillPicker');
  message.value = '所有信息在 /us'; message.fire('input');
  assert.equal(picker.hidden, false);
  assert.deepEqual(picker.children.map(node => node.textContent.split(' · ')[0]), ['profile-use', 'mail-use']);
  message.fire('keydown', { key: 'ArrowDown' }); message.fire('keydown', { key: 'ArrowUp' });
  message.fire('keydown', { key: 'Tab' });
  assert.equal(message.value, '所有信息在 /profile-use ');
  assert.equal(picker.hidden, true);
  // Esc closes the list without changing the text.
  message.value = '所有信息在 /profile-use 和 /ma'; message.fire('input');
  assert.equal(picker.hidden, false);
  message.fire('keydown', { key: 'Escape' });
  assert.equal(picker.hidden, true);
  assert.equal(message.value, '所有信息在 /profile-use 和 /ma');
  // Enter picks while the list is open, and does not send.
  message.fire('input'); message.fire('keydown', { key: 'Enter' });
  assert.equal(message.value, '所有信息在 /profile-use 和 /mail-use ');
  assert.equal(f.calls.filter(c => c.operation === 'conversationSend').length, 0);
  // Sent: the skill picked last is the one used; other `/words` stay text.
  message.value = '所有信息在 /profile-use, see /usr/bin'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  assert.deepEqual(f.calls.filter(c => c.operation === 'conversationSkillSelect').map(c => c.params.skillId), ['p']);
  assert.equal(f.calls.find(c => c.operation === 'conversationSend').params.skillId, 'snap-p');
  f.panel.close();
});

test('an unknown `/word` in the middle of a message is plain text, at the start it is refused', async () => {
  const f = await fixture({ status: () => ({ connected: true, agentPanel: { ...fullSupport, skillLibrary: true } }), skillList: async () => ({ success: true, result: { skills: [] } }),
    conversationSend: async params => ({ success: true, result: { conversation: chat('b'), message: { id: params.messageId, accepted: true, replayed: false } } }) }, { ab_panel_conversation: 'b' });
  await settle();
  f.nodes.get('message').value = 'open /etc/hosts please'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  assert.equal(f.calls.find(c => c.operation === 'conversationSend').params.skillId, undefined);
  f.nodes.get('message').value = '/nothing here'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  assert.equal(f.nodes.get('notice').textContent, en.panel_skill_unknown.message);
  assert.equal(f.calls.filter(c => c.operation === 'conversationSend').length, 1);
  f.panel.close();
});

// ---- One panel per tab (0.5.63) -----------------------------------------------

test('a tab\'s panel shows that tab\'s conversation and means that tab by this page', async () => {
  const tabs = { async get(id) { return { id, url: `https://shop.example/${id}`, title: `Tab ${id}` }; }, async query() { throw new Error('a tab panel never asks for the active tab'); }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const f = await fixture({ panelTab: 21, presence: true, tabs, session: { ab_panel_tab_21: 'b', ab_panel_tab_30: 'a' }, status: () => ({ connected: true, agentPanel: { ...fullSupport, pageContext: true } }) }, { ab_panel_conversation: 'a' });
  await settle(); await settle();
  assert.equal(f.nodes.get('conversation').value, 'b', 'its own conversation, not the last one used anywhere');
  assert.equal(f.nodes.get('pageTitle').textContent, 'Tab 21');
  assert.equal(f.presence.posted.at(-1).tabId, 21);
  // Choosing a conversation another tab shows moves it here.
  f.panel.select('a'); await settle(); await settle();
  assert.deepEqual(f.session.data, { ab_panel_tab_21: 'a' });
  assert.ok(!f.writes.some(w => 'ab_panel_conversation' in w));
  // A new conversation leaves the tab unbound until it exists.
  f.panel.select(''); await settle();
  assert.deepEqual(f.session.data, {});
  // A notification bound a conversation to this tab while the panel was open.
  f.storageChange({ ab_panel_tab_21: { newValue: 'b' } }); await settle();
  assert.equal(f.nodes.get('conversation').value, 'b');
  // Another tab's change is not this panel's business.
  f.storageChange({ ab_panel_tab_30: { newValue: 'a' } }); await settle();
  assert.equal(f.nodes.get('conversation').value, 'b');
  f.panel.close();
});

test('a panel opened on a new tab starts a new conversation', async () => {
  const tabs = { async get(id) { return { id, url: 'https://new.example/', title: 'New' }; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const f = await fixture({ panelTab: 44, tabs, session: { ab_panel_tab_21: 'b' } }, { ab_panel_conversation: 'b' });
  await settle();
  assert.equal(f.nodes.get('conversation').value, '');
  f.panel.close();
});

// ---- The agent works in the panel's tab (0.5.63) ------------------------------

function lendingFixture({ url = 'https://shop.example/cart', sync, grantsAtStart = [], conversationSend } = {}) {
  const grantCalls = [], state = { grants: [...grantsAtStart] };
  const tabs = { async get(id) { return { id, url, title: 'Cart' }; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const tabGrant = request => {
    grantCalls.push(request);
    if (request.action === 'list') return { success: true, grants: state.grants };
    if (request.action === 'grant') { const grant = { tabId: request.tabId, conversationId: request.conversationId, url, title: 'Cart' }; state.grants = [...state.grants.filter(g => g.tabId !== request.tabId), grant]; return { success: true, grant, service: { granted: true, tabId: request.tabId, adopted: true } }; }
    state.grants = state.grants.filter(g => request.tabId !== undefined ? g.tabId !== request.tabId : g.conversationId !== request.conversationId);
    return { success: true, grant: null, service: { granted: false } };
  };
  return { grantCalls, state, options: { panelTab: 21, tabs, sync, session: { ab_panel_tab_21: 'b' }, tabGrant, status: () => grantSupport, ...(conversationSend ? { conversationSend } : {}) } };
}

test('a tab\'s panel lends its tab to its conversation by itself, and a take-back moves it to its own window', async () => {
  const l = lendingFixture();
  const f = await fixture(l.options, {});
  await settle(); await settle(); await settle();
  assert.deepEqual(l.grantCalls.filter(c => c.action === 'grant'), [{ type: 'ab-tab-grant', action: 'grant', tabId: 21, conversationId: 'b', conversationTitle: 'b' }]);
  assert.equal(f.nodes.get('grantBar').hidden, false);
  assert.equal(f.nodes.get('workRow').hidden, false);
  assert.equal(f.nodes.get('workIn').value, 'tab');
  // Take back: revoked, the conversation now works in the agent's window, and
  // later reads do not lend the tab again.
  f.nodes.get('grantRevoke').fire('click'); await settle(); await settle(); await settle();
  assert.ok(l.grantCalls.some(c => c.action === 'revoke' && c.conversationId === 'b'));
  assert.deepEqual(f.writes.filter(w => w.ab_panel_work_in).at(-1).ab_panel_work_in, { b: 'window' });
  assert.equal(f.nodes.get('workIn').value, 'window');
  assert.equal(f.nodes.get('workNote').textContent, 'You took the tab back. The agent works in its own window now.');
  await f.panel.refresh(); await settle(); await settle();
  assert.equal(l.grantCalls.filter(c => c.action === 'grant').length, 1);
  // Choosing "this tab" again lends it again.
  f.nodes.get('workIn').value = 'tab'; f.nodes.get('workIn').fire('change'); await settle(); await settle(); await settle();
  assert.equal(l.grantCalls.filter(c => c.action === 'grant').length, 2);
  f.panel.close();
});

test('a page that cannot be lent leaves the agent in its own window and says so', async () => {
  const l = lendingFixture({ url: 'chrome://settings/' });
  const f = await fixture(l.options, {});
  await settle(); await settle();
  assert.equal(l.grantCalls.filter(c => c.action === 'grant').length, 0);
  assert.equal(f.nodes.get('workNote').textContent, en.panel_work_in_window_page.message);
  f.panel.close();
  const store = lendingFixture({ url: 'https://chromewebstore.google.com/detail/x' });
  const g = await fixture(store.options, {});
  await settle(); await settle();
  assert.equal(g.nodes.get('workNote').textContent, en.panel_work_in_window_web_store.message);
  g.panel.close();
});

test('"its own window" in Options keeps the tab to the user until a conversation asks for it', async () => {
  const l = lendingFixture({ sync: { ab_work_in: 'window' } });
  const f = await fixture(l.options, {});
  await settle(); await settle();
  assert.equal(l.grantCalls.filter(c => c.action === 'grant').length, 0);
  assert.equal(f.nodes.get('workIn').value, 'window');
  assert.equal(f.nodes.get('workNote').hidden, true);
  f.panel.close();
});

test('a tab still lent to the conversation it showed before is moved to the one it shows now', async () => {
  const l = lendingFixture({ grantsAtStart: [{ tabId: 21, conversationId: 'a', url: 'https://shop.example/cart', title: 'Cart' }] });
  const f = await fixture(l.options, {});
  await settle(); await settle(); await settle();
  const order = l.grantCalls.filter(c => c.action !== 'list').map(c => [c.action, c.tabId, c.conversationId]);
  assert.deepEqual(order, [['revoke', 21, undefined], ['grant', 21, 'b']]);
  f.panel.close();
});

test('the first message of a new conversation in a tab\'s panel already runs in that tab', async () => {
  const l = lendingFixture({ conversationSend: async params => ({ success: true, result: { conversation: chat('new'), message: { id: params.messageId, accepted: true, replayed: false } } }) });
  l.options.session = {};
  l.options.conversationCreate = async () => ({ success: true, result: { conversation: { ...chat('new'), agentId: 'codex' } } });
  const f = await fixture(l.options, {});
  await settle();
  f.nodes.get('message').value = 'fill the form'; await f.panel.send(); await settle();
  const send = f.calls.find(c => c.operation === 'conversationSend');
  assert.deepEqual(send.params.tabGrant, { tabId: 21, url: 'https://shop.example/cart', title: 'Cart' });
  const grantIndex = f.calls.findIndex(c => c.action === 'grant'), sendIndex = f.calls.indexOf(send);
  assert.ok(grantIndex >= 0 && grantIndex < sendIndex, 'lent before the first message');
  assert.equal(f.session.data.ab_panel_tab_21, 'new');
  f.panel.close();
});

test('an update blocked by an older build names its conversations; Open finds one started on another site', async () => {
  const id = '12b6a0cd-eca8-4462-a809-16db0688704c';
  const listed = [{ ...chat('b'), site: 'shop.example' }, { ...chat(id, 'waitingInput'), title: 'Old approval', site: 'other.example' }];
  const tabs = { async query() { return [{ id: 7, url: 'https://shop.example/cart', title: 'Cart' }]; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const f = await fixture({ tabs, status: () => ({ connected: true, agentPanel: { ...fullSupport, pageContext: true } }),
    conversationList: async () => ({ success: true, result: { conversations: listed } }),
    conversationRead: async params => ({ success: true, result: { conversation: listed.find(c => c.id === params.conversationId) || chat(params.conversationId), journal: { events: [], cursor: 0, more: false } } }),
    conversationSend: async () => ({ success: false, error: `Another agent-panel build is running for this profile; stop its conversations before updating (still working or waiting for you: “Old approval”) [blocking:${id}]` }),
    conversationStop: async () => ({ success: true, result: { conversation: chat(id, 'stopping') } }) }, { ab_panel_conversation: 'b' });
  await settle();
  f.nodes.get('message').value = 'go'; f.nodes.get('composer').fire('submit'); await settle(); await settle();
  const notice = f.nodes.get('notice');
  assert.equal(notice.hidden, false);
  assert.equal(notice.children[0].textContent, en.panel_old_build_busy.message);
  const row = notice.children[1];
  assert.match(row.textContent, /Old approval/);
  const [open, stop] = row.children;
  assert.deepEqual([open.textContent, stop.textContent], ['Open', 'Stop it']);
  // A terminal command takes over without answering or stopping each one.
  const force = notice.children[2];
  assert.equal(force.children[1].textContent, 'chrome-use upgrade -f');
  let copied = '';
  globalThis.navigator ??= {};
  const clipboard = Object.getOwnPropertyDescriptor(globalThis.navigator, 'clipboard');
  Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { async writeText(text) { copied = text; } } });
  const copy = force.children[2];
  copy.fire('click'); await settle();
  assert.equal(copied, 'chrome-use upgrade -f');
  assert.equal(copy.textContent, en.panel_copied.message);
  if (clipboard) Object.defineProperty(globalThis.navigator, 'clipboard', clipboard); else delete globalThis.navigator.clipboard;
  // This site's list does not show it; opening switches the list to all sites.
  open.fire('click'); await settle(); await settle();
  assert.equal(f.nodes.get('conversation').value, id);
  assert.ok(f.writes.some(w => w.ab_panel_conversation_scope === 'all'));
  assert.equal(notice.hidden, true);
  f.panel.close();
});

// ---- Inspect & Ask → side-panel chat (0.5.69) ---------------------------------

const pickedChip = (id, selector, extra = {}) => ({ id, documentId: 'd1', pageUrl: 'https://shop.example/21', label: `button "${id}"`, component: null, selector, frames: [], shadowHosts: [], foreignFrame: null, text: id, source: null, note: '', html: '', rect: null, image: null, stale: false, addedAt: 1, ...extra });

test('picked elements wait as chips in their tab\'s composer and ride in the next message', async () => {
  const tabs = { async get(id) { return { id, url: `https://shop.example/${id}`, title: `Tab ${id}` }; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const sent = [], put = [];
  const f = await fixture({
    panelTab: 21, tabs,
    session: { ab_panel_tab_21: 'b', ab_inspect_tab_21: { items: [pickedChip('save', 'form > button.save', { image: 'data:image/png;base64,iVBORw0KGgo=', stale: true })], question: 'Why is it gray?' } },
    status: () => ({ connected: true, agentPanel: { ...fullSupport, pageContext: true, imageAttachments: true } }),
    encodeImage: async blob => ({ name: 'Image', mediaType: blob.type, data: 'shot' }),
    conversationAttachmentPut: async params => { put.push(params); return { success: true, result: { attachment: { id: `att${put.length}` } } }; },
    conversationSend: async params => { sent.push(params); return { success: true, result: { conversation: chat('b') } }; },
  });
  await settle(); await settle();
  const chips = f.nodes.get('inspectChips');
  assert.equal(chips.hidden, false);
  assert.equal(chips.children.length, 1);
  assert.equal(f.nodes.get('message').value, 'Why is it gray?', 'the question from the picker goes into the composer');
  assert.equal(f.session.data.ab_inspect_tab_21.question, '', 'and is taken out so a reload does not add it again');
  // The panel was open already: a new pick adds a chip and sends nothing.
  const record = { items: [...f.session.data.ab_inspect_tab_21.items, pickedChip('more', 'a.more')], question: '' };
  f.session.data.ab_inspect_tab_21 = record;
  f.storageChange({ ab_inspect_tab_21: { newValue: record } }); await settle();
  assert.equal(chips.children.length, 2);
  assert.equal(sent.length, 0);
  // Removing one.
  chips.children[1].querySelectorAll('button')[0].fire('click'); await settle(); await settle();
  assert.deepEqual(f.session.data.ab_inspect_tab_21.items.map(c => c.id), ['save']);
  assert.equal(chips.children.length, 1);
  // Copy hands the same context to another tool, without sending (#680).
  assert.equal(f.nodes.get('chipActions').hidden, false);
  let copied = '';
  globalThis.navigator ??= {};
  const clipboard = Object.getOwnPropertyDescriptor(globalThis.navigator, 'clipboard');
  Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { async writeText(text) { copied = text; } } });
  f.nodes.get('copyElements').fire('click'); await settle();
  assert.ok(copied.startsWith('[Page elements the user picked'), copied);
  assert.match(copied, /selector: `form > button\.save`/);
  assert.match(copied, /screenshot: taken in the side panel, not part of this copied text/);
  assert.equal(f.nodes.get('copyElements').textContent, en.panel_copied.message);
  f.nodes.get('copyInput').fire('click'); await settle();
  assert.ok(copied.startsWith('Why is it gray?\n\n[Page elements the user picked'), copied);
  if (clipboard) Object.defineProperty(globalThis.navigator, 'clipboard', clipboard); else delete globalThis.navigator.clipboard;
  assert.equal(sent.length, 0, 'copying sends nothing');
  await f.panel.send(); await settle();
  assert.equal(sent.length, 1);
  assert.ok(sent[0].text.startsWith('Why is it gray?\n\n[Page elements the user picked'), sent[0].text);
  assert.match(sent[0].text, /in tab "chrome-tab:21"/);
  assert.match(sent[0].text, /selector: `form > button\.save`/);
  assert.match(sent[0].text, /screenshot: attached image 1/);
  assert.match(sent[0].text, /stale: picked before the page changed/);
  assert.doesNotMatch(sent[0].text, /a\.more/);
  assert.equal(sent[0].attachments.length, 1);
  assert.equal(put[0].data, 'shot');
  assert.equal(f.session.data.ab_inspect_tab_21, undefined, 'sent chips leave the composer');
  assert.equal(chips.hidden, true);
  assert.equal(f.nodes.get('message').value, '');
  assert.equal(f.nodes.get('chipActions').hidden, true);
  f.panel.close();
});

test('chips alone can be sent, and a failed send keeps them', async () => {
  const tabs = { async get(id) { return { id, url: `https://shop.example/${id}`, title: `Tab ${id}` }; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  let fail = true;
  const sent = [];
  const f = await fixture({
    panelTab: 21, tabs,
    session: { ab_panel_tab_21: 'b', ab_inspect_tab_21: { items: [pickedChip('save', 'form > button.save')], question: '' } },
    status: () => ({ connected: true, agentPanel: { ...fullSupport, pageContext: true } }),
    conversationSend: async params => { sent.push(params); if (fail) throw new Error('lost'); return { success: true, result: { conversation: chat('b') } }; },
  });
  await settle(); await settle();
  assert.equal(f.nodes.get('message').value, '');
  await assert.rejects(f.panel.send());
  assert.equal(f.session.data.ab_inspect_tab_21.items.length, 1, 'a failed send keeps the chips');
  assert.match(sent[0].text, /^\[Page elements the user picked/);
  f.panel.close();
});

test('another tab\'s chips never show here', async () => {
  const tabs = { async get(id) { return { id, url: `https://shop.example/${id}`, title: `Tab ${id}` }; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const f = await fixture({ panelTab: 21, tabs, hiddenNodes: ['inspectChips'], session: { ab_inspect_tab_30: { items: [pickedChip('x', '#x')], question: 'not mine' } } });
  await settle();
  assert.equal(f.nodes.get('inspectChips').hidden, true);
  f.storageChange({ ab_inspect_tab_30: { newValue: { items: [pickedChip('y', '#y')], question: 'still not mine' } } }); await settle();
  assert.equal(f.nodes.get('inspectChips').hidden, true);
  assert.equal(f.nodes.get('message').value, '');
  f.panel.close();
});

// ---- The Pick button (0.5.69) -----------------------------------------------------

const pickTabs = url => ({ async get(id) { return { id, url, title: `Tab ${id}` }; }, onActivated: { addListener() {} }, onUpdated: { addListener() {} } });

test('the Pick button starts a picker on its tab, shows it is active, and cancels on a second click or Esc', async () => {
  const f = await fixture({ panelTab: 21, tabs: pickTabs('https://shop.example/21'), session: {} });
  await settle(); await settle();
  const pick = f.nodes.get('pickElement');
  assert.equal(pick.hidden, false);
  assert.equal(pick.disabled, false);
  pick.fire('click', { shiftKey: true }); await settle();
  assert.deepEqual(f.calls.find(c => c.type === 'ab-inspect-pick'), { type: 'ab-inspect-pick', tabId: 21, keep: true });
  assert.equal(f.nodes.get('pickHint').hidden, false, 'the panel says what to do on the page');
  // The background confirms, then ends it (the pick is done).
  f.storageChange({ ab_inspect_pick_21: { newValue: { instanceId: 'x', keep: true } } }); await settle();
  f.storageChange({ ab_inspect_pick_21: { newValue: undefined } }); await settle();
  assert.equal(f.nodes.get('pickHint').hidden, true);
  // Started again, then cancelled with Esc from the panel.
  pick.fire('click', {}); await settle();
  for (const fn of f.document.listeners.keydown || []) fn({ key: 'Escape', preventDefault() {} });
  await settle();
  assert.ok(f.calls.some(c => c.type === 'ab-inspect-pick-cancel' && c.tabId === 21));
  assert.equal(f.nodes.get('pickHint').hidden, true);
  f.panel.close();
});

test('a page that cannot be picked disables the button; a site without access asks Chrome once', async () => {
  const off = await fixture({ panelTab: 21, tabs: pickTabs('chrome://settings/'), session: {} });
  await settle(); await settle();
  assert.equal(off.nodes.get('pickElement').disabled, true);
  off.panel.close();
  const asked = [];
  let replies = [{ success: true, needsPermission: 'https://shop.example/*' }, { success: true, started: true }];
  const f = await fixture({ panelTab: 21, tabs: pickTabs('https://shop.example/21'), session: {}, pick: () => replies.shift(), permissions: { async request(p) { asked.push(p); return true; } } });
  await settle(); await settle();
  await f.panel.togglePick({});
  assert.deepEqual(asked, [{ origins: ['https://shop.example/*'] }]);
  assert.equal(f.nodes.get('pickHint').hidden, false);
  f.panel.close();
  replies = [{ success: true, needsPermission: 'https://shop.example/*' }];
  const g = await fixture({ panelTab: 21, tabs: pickTabs('https://shop.example/21'), session: {}, pick: () => replies.shift(), permissions: { async request() { return false; } } });
  await settle(); await settle();
  await assert.rejects(g.panel.togglePick({}), /access to this site/);
  assert.equal(g.nodes.get('pickHint').hidden, true);
  g.panel.close();
});

test('a chip that lands is highlighted and announced; Delete on a focused chip removes it', async () => {
  const f = await fixture({ panelTab: 21, tabs: pickTabs('https://shop.example/21'), session: {} });
  await settle(); await settle();
  const record = { items: [pickedChip('save', 'form > button.save', { pending: { image: true, source: true }, source: { path: 'src/pages/Order.tsx', line: 42, kind: 'insp' } })], question: '' };
  f.session.data.ab_inspect_tab_21 = record;
  f.storageChange({ ab_inspect_tab_21: { newValue: record } }); await settle(); await settle();
  const chip = f.nodes.get('inspectChips').children[0];
  assert.match(chip.className, /page-chip inspect-chip landed/);
  assert.ok(chip.children.some(c => c.className === 'inspect-chip-src' && c.textContent === 'Order.tsx:42'));
  assert.ok(chip.children.some(c => c.className === 'inspect-chip-pending'), 'a placeholder while the screenshot comes');
  assert.match(f.nodes.get('chipLive').textContent, /save/);
  chip.fire('focus'); assert.equal(f.nodes.get('chipCard').hidden, false);
  chip.fire('keydown', { key: 'Delete' }); await settle(); await settle();
  assert.equal(f.session.data.ab_inspect_tab_21, undefined);
  assert.equal(f.nodes.get('chipCard').hidden, true);
  f.panel.close();
});

test('the empty state says the agent works in this tab, or in its own window when set so', async () => {
  const f = await fixture({ panelTab: 21, tabs: pickTabs('https://shop.example/21'), session: {} });
  await settle(); await settle();
  assert.match(f.nodes.get('emptyDetail').textContent, /works in this tab by default/);
  f.panel.close();
  const g = await fixture({ panelTab: 21, tabs: pickTabs('https://shop.example/21'), session: {}, sync: { ab_work_in: 'window' } });
  await settle(); await settle();
  assert.match(g.nodes.get('emptyDetail').textContent, /its own browser window/);
  g.panel.close();
  const h = await fixture({ mode: 'tab' });
  await settle();
  assert.match(h.nodes.get('emptyDetail').textContent, /its own browser window/);
  h.panel.close();
});

test('missing-host guidance appears below the error and clears for other failures', async () => {
  const original = 'Specified native messaging host not found.';
  const f = await fixture({ status: () => ({ connected: false, connectionError: original }) });
  assert.equal(f.nodes.get('connection').textContent, original);
  assert.equal(f.nodes.get('hostHint').hidden, false);
  assert.equal(f.nodes.get('hostHint').children[0].textContent, en.popup_hint_install_host.message);
  assert.equal(f.nodes.get('hostHint').children[1].textContent, 'chrome-use extension install');
  assert.equal(f.nodes.get('hostHint').children[2].textContent, en.panel_copy_command.message);
  f.hostUpdate({ connected: false, connectionError: 'Access denied' });
  assert.equal(f.nodes.get('hostHint').hidden, true);
  assert.equal(f.nodes.get('hostHint').children.length, 0);
  f.panel.close();
});
for (const phase of ['failed', 'interrupted', 'idle']) {
  test(`sending after a ${phase} paused turn starts the new message in the same conversation`, async () => {
    const paused = { ...chat('a', phase), agentId: 'codex', model: 'gpt-6.1-sol', queuePaused: true, queued: 1,
      lastError: phase === 'failed' ? 'Selected model is at capacity. Please try a different model.' : null };
    const f = await fixture({
      conversationList: () => ({ success: true, result: { conversations: [paused] } }),
      conversationRead: () => ({ success: true, result: { conversation: paused, journal: { events: [], cursor: 0, more: false } } }),
      conversationSend: () => ({ success: true, result: { conversation: { ...paused, phase: 'running' } } }),
    }, { ab_panel_conversation: 'a' });
    f.nodes.get('message').value = 'continue';
    f.nodes.get('composer').fire('submit'); await settle(); await settle();
    const sends = f.calls.filter(call => ['conversationSend', 'conversationSendDirect'].includes(call.operation));
    assert.equal(sends.length, 1);
    assert.equal(sends[0].operation, 'conversationSend');
    assert.equal(sends[0].params.conversationId, 'a');
    assert.equal(sends[0].params.text, 'continue');
    assert.ok(!f.calls.some(call => ['conversationCreate', 'conversationResume'].includes(call.operation)));
    f.panel.close();
  });
}

test('conversation window action preserves selected chat and reports unconfirmed navigation', async () => {
  const f = await fixture(); f.panel.select('a'); await settle();
  f.nodes.get('conversationButton').fire('click');
  const row = f.nodes.get('conversationItems').children[2];
  row.children[1].fire('click'); await settle();
  assert.equal(f.nodes.get('conversation').value, 'a');
  assert.equal(f.calls.at(-1).type, 'ab-conversation-window');
  assert.equal(f.calls.at(-1).conversationId, 'b');
  assert.equal(f.calls.at(-1).destination, 'initial');
  row.children[2].fire('click'); await settle();
  assert.equal(f.calls.at(-1).destination, 'final');
  assert.equal(f.nodes.get('conversation').value, 'a');
  assert.equal(f.nodes.get('notice').textContent, en.panel_window_unconfirmed.message);
  assert.equal(f.nodes.get('notice').hidden, false);
  f.panel.close();
});

test('a deleted chat is removed while saved page cleanup failure stays visible', async () => {
  const f=await fixture({status:()=>({connected:true,agentPanel:{...fullSupport,conversationDelete:true}}),
    conversationDelete:params=>({success:true,result:{deleted:{id:params.conversationId,cliHistory:{removed:[],failed:[]},browserSession:{closed:true},pageCleanup:{error:'storage unavailable'}}}}),
  },{ab_panel_conversation:'b'});
  f.chats.splice(1,1);f.nodes.get('confirmDelete').fire('click');await settle();await settle();
  assert.equal(f.nodes.get('conversation').value,'');
  assert.match(f.nodes.get('notice').textContent,/Saved page address cleanup failed: storage unavailable/);
  f.panel.close();
});


test('row deletion confirms the chosen conversation and cancellation leaves it intact', async () => {
  let deleted = [];
  const f = await fixture({ status: () => ({ connected: true, agentPanel: { ...fullSupport, conversationDelete: true } }),
    conversationDelete: params => { deleted.push(params.conversationId); return { success: true, result: { deleted: { id: params.conversationId, cliHistory: { failed: [] }, browserSession: { closed: true } } } }; },
  });
  f.panel.select('a'); await settle();
  f.nodes.get('conversationButton').fire('click');
  let rows = f.nodes.get('conversationItems').children;
  assert.equal(rows[1].children[3].disabled, true); // Running chat must stop first.
  rows[2].children[3].fire('click'); await settle();
  assert.equal(f.nodes.get('conversation').value, 'b');
  assert.equal(f.nodes.get('deleteConfirm').hidden, false);
  assert.deepEqual(deleted, []);
  f.nodes.get('cancelDelete').fire('click');
  assert.equal(f.nodes.get('deleteConfirm').hidden, true);
  f.nodes.get('conversationButton').fire('click');
  rows = f.nodes.get('conversationItems').children;
  rows[2].children[3].fire('click'); await settle();
  f.nodes.get('confirmDelete').fire('click'); await settle();
  assert.deepEqual(deleted, ['b']);
  f.panel.close();
});
