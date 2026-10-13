import assert from 'node:assert/strict';
import test from 'node:test';
import { createPanel } from './sidepanel.js';

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
  fire(type) { this.listeners[type]?.({ preventDefault() {} }); }
}
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const settle = () => new Promise(resolve => setImmediate(resolve));
const fullSupport = {schemaVersion:1,mode:'full',modelInspection:true};
const chat = (id, phase = 'idle') => ({ id, title: id, phase, queued: 0, queuePaused: false, model: '' });

async function fixture(overrides = {}, saved = {}) {
  const nodes = new Map(), calls = [], writes = [];
  let hostState;
  const chats = [chat('a', 'running'), chat('b')];
  const runtime = {
    onMessage: { addListener(listener) { hostState = listener; } },
    async sendMessage(request) {
      if (request.type === 'ab-status') return overrides.status ? {agentPanel:fullSupport,...overrides.status()} : { connected: true,agentPanel:fullSupport };
      calls.push(request);
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
  const document = { createElement: tag => new Node(tag), createTextNode: text => Object.assign(new Node('#text'), { textContent: text }), getElementById(id) { if (!nodes.has(id)) nodes.set(id, new Node()); return nodes.get(id); } };
  const panel = createPanel({ document, runtime, tabs: overrides.tabs, storage: { local: { async get() { return saved; }, async set(values) { writes.push(structuredClone(values)); } } }, schedule() {}, cancel() {} });
  await panel.start(); await settle();
  return { panel, nodes, calls, chats, writes, hostUpdate(state) { hostState({type:'ab-host-state',agentPanel:fullSupport,...state}); } };
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
  assert.deepEqual(lines, ['v0.50.0 · Signed in (chatgpt) · 2 models', 'Configuration found; program not installed']);
  assert.equal(f.calls.filter(call => call.operation === 'agentInspect').length, 1);
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
  const titles = () => f.nodes.get('conversationItems').children.slice(1).map(item => item.children[0]?.textContent);
  f.nodes.get('conversationButton').fire('click');
  assert.equal(f.nodes.get('conversationMenu').hidden, false);
  assert.equal(f.nodes.get('scopeSite').textContent, 'This site · x.com');
  assert.deepEqual(titles(), ['reply on x', 'mobile x']);
  f.nodes.get('scopeAll').fire('click');
  assert.deepEqual(titles(), ['reply on x', 'mobile x', 'repos', 'no site']);
  assert.ok(f.writes.some(w => w.ab_panel_conversation_scope === 'all'));
  f.nodes.get('conversationItems').children[3].fire('click'); await settle();
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
