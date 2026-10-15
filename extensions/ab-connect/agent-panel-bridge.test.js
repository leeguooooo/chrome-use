import test from 'node:test';
import assert from 'node:assert/strict';
import { AgentPanelBridge, isAgentPanelSender } from './agent-panel-bridge.js';

test('default browser timers retain the global receiver rather than the bridge instance', async () => {
  const originalSet = globalThis.setTimeout, originalClear = globalThis.clearTimeout;
  const messages = [];
  try {
    globalThis.setTimeout = function () { assert.equal(this,globalThis); return 42; };
    globalThis.clearTimeout = function (timer) { assert.equal(this,globalThis); assert.equal(timer,42); };
    const bridge = new AgentPanelBridge({send:message=>messages.push(message)});
    const pending = bridge.request('discover');
    bridge.receive({method:'agentPanelReply',requestId:messages[0].requestId,result:{schemaVersion:1,agents:[]}});
    assert.deepEqual((await pending).agents,[]);
  } finally { globalThis.setTimeout = originalSet; globalThis.clearTimeout = originalClear; }
});

test('queue mutation replies must prove the requested original and replacement identities', async () => {
  for (const operation of ['conversationQueueEdit','conversationQueueRemove']) {
    const {bridge,messages}=setup();
    const pending=bridge.request(operation,{conversationId:'chat',messageId:'original',replacementId:'replacement'});
    bridge.receive({method:'agentPanelReply',requestId:messages[0].requestId,result:{schemaVersion:1,conversation:{id:'chat',phase:'running'},queueChange:{messageId:'wrong',replacementId:'replacement',kind:operation==='conversationQueueEdit'?'edited':'removed'}}});
    await assert.rejects(pending,/Malformed/);
  }
});

test('message status validates identity and never interprets an absent ID as rejection', async () => {
  for (const [message, valid] of [
    [{ id: 'message', known: true, status: 'previouslyAccepted' }, true],
    [{ id: 'message', known: false, status: 'unconfirmed' }, true],
    [{ id: 'wrong', known: true, status: 'active' }, false],
    [{ id: 'message', known: false, status: 'rejected' }, false],
    [{ id: 'message', known: true, status: 'unconfirmed' }, false],
  ]) {
    const { bridge, messages } = setup();
    const reply = bridge.request('conversationMessageStatus', { conversationId: 'chat', messageId: 'message' });
    bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, result: { schemaVersion: 1, conversation: { id: 'chat', phase: 'idle' }, message } });
    if (valid) assert.deepEqual((await reply).message, message);
    else await assert.rejects(reply, /Malformed/);
  }
});

function setup(options = {}) {
  const messages = [], timers = new Map();
  let next = 0;
  const bridge = new AgentPanelBridge({
    send: message => messages.push(message),
    schedule: fn => { timers.set(++next, fn); return next; },
    cancel: id => timers.delete(id), ...options,
  });
  return { bridge, messages, timers };
}

test('out-of-order native replies retain their request identity', async () => {
  const { bridge, messages, timers } = setup();
  const a = bridge.request('discover'), b = bridge.request('discover');
  bridge.receive({ method: 'agentPanelReply', requestId: messages[1].requestId, result: { schemaVersion: 1, agents: [] } });
  bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, error: 'Host discovery failed' });
  assert.deepEqual(await b, { schemaVersion: 1, agents: [] });
  await assert.rejects(a, /Host discovery failed/);
  assert.equal(timers.size, 0);
});

test('disconnect rejects work; stale replies cannot settle a new connection', async () => {
  const { bridge, messages } = setup();
  const old = bridge.request('discover');
  bridge.disconnect('Connection ended');
  await assert.rejects(old, /Connection ended/);
  const current = bridge.request('discover');
  assert.notEqual(messages[0].requestId, messages[1].requestId);
  bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, result: 'stale' });
  assert.equal(bridge.pending.size, 1);
  bridge.receive({ method: 'agentPanelReply', requestId: messages[1].requestId, result: { schemaVersion: 1, agents: [] } });
  assert.deepEqual(await current, { schemaVersion: 1, agents: [] });
});

test('timeout names uncertainty and never resends', async () => {
  const { bridge, messages, timers } = setup();
  const result = bridge.request('discover');
  [...timers.values()][0]();
  await assert.rejects(result, /not replayed/);
  assert.equal(messages.length, 1);
  assert.equal(bridge.pending.size, 0);
  assert.equal(bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, result: 'late' }), true);
});

test('malformed native reply fails instead of claiming success', async () => {
  const { bridge, messages } = setup();
  const result = bridge.request('discover');
  bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId });
  await assert.rejects(result, /Malformed/);
});

test('unsupported schema and null results do not report success', async () => {
  for (const result of [null, { schemaVersion: 2, agents: [] }, { schemaVersion: 1, agents: null }]) {
    const { bridge, messages } = setup();
    const pending = bridge.request('discover');
    bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, result });
    await assert.rejects(pending, /Malformed/);
  }
});

test('worker recreation cannot reuse the previous worker request ids', async () => {
  const a = setup(), b = setup();
  const pendingA = a.bridge.request('discover'), pendingB = b.bridge.request('discover');
  assert.notEqual(a.messages[0].requestId, b.messages[0].requestId);
  a.bridge.disconnect(); b.bridge.disconnect();
  await assert.rejects(pendingA, /disconnected/);
  await assert.rejects(pendingB, /disconnected/);
});

test('failed send and disconnected port do not leave a pending request', async () => {
  const { bridge, timers } = setup({ send() { throw new Error('No host'); } });
  await assert.rejects(bridge.request('discover'), /No host/);
  assert.equal(bridge.pending.size, 0);
  assert.equal(timers.size, 0);
});

test('unknown operations and overload fail before reaching the host', async () => {
  const { bridge, messages } = setup({ maxPending: 1 });
  await assert.rejects(bridge.request('arbitrary-shell'), /Unsupported/);
  const pending = bridge.request('discover');
  await assert.rejects(bridge.request('discover'), /Too many/);
  assert.equal(messages.length, 1);
  bridge.disconnect();
  await assert.rejects(pending, /disconnected/);
});

test('non-panel CDP and health messages pass through unchanged', () => {
  const { bridge } = setup();
  assert.equal(bridge.receive({ method: 'pong' }), false);
  assert.equal(bridge.receive({ method: 'forwardCDPCommand', params: {} }), false);
});

test('only packaged extension pages may invoke native agent services', () => {
  const runtime = { id: 'own', getURL: path => `chrome-extension://own/${path}` };
  assert.equal(isAgentPanelSender({ id: 'own', url: runtime.getURL('sidepanel.html') }, runtime), true);
  assert.equal(isAgentPanelSender({ id: 'own', url: runtime.getURL('options.html') }, runtime), true);
  for (const sender of [
    { id: 'other', url: runtime.getURL('sidepanel.html') },
    { id: 'own', url: 'https://example.com/' },
    { id: 'own', url: runtime.getURL('sidepanel.html'), tab: { id: 1 } },
    { id: 'own', url: runtime.getURL('sidepanel.html') + '?spoof' },
  ]) assert.equal(isAgentPanelSender(sender, runtime), false);
});

test('configuration replies must match the requested operation and real outcome', async () => {
  const { bridge, messages } = setup();
  const read = bridge.request('configRead', { agentId: 'codex' });
  bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId,
    result: { schemaVersion: 1, configuration: { agentId: 'codex', fields: [], fingerprint: 'fixture' } } });
  assert.equal((await read).configuration.agentId, 'codex');
  const apply = bridge.request('configApply', { agentId: 'codex', planId: 'fixture' });
  bridge.receive({ method: 'agentPanelReply', requestId: messages[1].requestId,
    result: { schemaVersion: 1, configuration: { agentId: 'codex', planId: 'fixture', files: [], phase: 'planned' } } });
  await assert.rejects(apply, /Malformed/);
  const restore = bridge.request('configRestore', { agentId: 'codex', planId: 'fixture' });
  bridge.receive({ method: 'agentPanelReply', requestId: messages[2].requestId,
    result: { schemaVersion: 1, configuration: { agentId: 'codex', planId: 'fixture', files: [], phase: 'restoreConflicts', conflicts: [['model']] } } });
  assert.deepEqual((await restore).configuration.conflicts, [['model']]);
});

test('configuration mutation timeout and disconnect retain uncertainty and never replay', async () => {
  const { bridge, messages, timers } = setup();
  const apply = bridge.request('configApply', { agentId: 'codex', planId: 'fixture' });
  [...timers.values()][0]();
  await assert.rejects(apply, /outcome could not be confirmed/);
  const restore = bridge.request('configRestore', { agentId: 'codex', planId: 'fixture' });
  bridge.disconnect('Host ended');
  await assert.rejects(restore, /outcome is unconfirmed/);
  assert.equal(messages.length, 2);
});

test('a reply for a different agent or plan cannot confirm a configuration effect', async () => {
  for (const configuration of [
    { agentId: 'claude', planId: 'requested', files: [], phase: 'applied' },
    { agentId: 'codex', planId: 'different', files: [], phase: 'applied' },
  ]) {
    const { bridge, messages } = setup();
    const result = bridge.request('configApply', { agentId: 'codex', planId: 'requested' });
    bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, result: { schemaVersion: 1, configuration } });
    await assert.rejects(result, /Malformed/);
  }
});

test('conversation timeout and disconnect never cause message resubmission', async () => {
  const { bridge, messages, timers } = setup();
  const result = bridge.request('conversationSend', { conversationId: 'chat', messageId: 'stable', text: 'fixture' });
  [...timers.values()][0]();
  await assert.rejects(result, /no message was replayed/);
  assert.equal(messages.length, 1);
  const stopping = bridge.request('conversationStop', { conversationId: 'chat' });
  bridge.disconnect();
  await assert.rejects(stopping, /without resending/);
});

test('conversation replies must match both chat and message identities', async () => {
  for (const result of [
    { schemaVersion: 1, conversation: { id: 'other', phase: 'running' }, message: { id: 'message', accepted: true, replayed: false } },
    { schemaVersion: 1, conversation: { id: 'chat', phase: 'running' }, message: { id: 'other', accepted: true, replayed: false } },
  ]) {
    const { bridge, messages } = setup();
    const pending = bridge.request('conversationSend', { conversationId: 'chat', messageId: 'message', text: 'fixture' });
    bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, result });
    await assert.rejects(pending, /Malformed/);
  }
});

test('delete replies must name the requested conversation and report what was removed', async () => {
  const good = {id:'chat',cliHistory:{removed:['/x'],failed:[]},browserSession:{session:'panel-chat',closed:true}};
  for (const [deleted, valid] of [[good,true],[{...good,id:'other'},false],[{id:'chat'},false],[{...good,browserSession:{}},false]]) {
    const {bridge,messages}=setup();
    const pending=bridge.request('conversationDelete',{conversationId:'chat'});
    bridge.receive({method:'agentPanelReply',requestId:messages[0].requestId,result:{schemaVersion:1,deleted}});
    if (valid) assert.equal((await pending).deleted.id,'chat'); else await assert.rejects(pending,/Malformed/);
  }
});

test('slow operations wait longer than the native host does before giving up', () => {
  const delays = {};
  const bridge = new AgentPanelBridge({ send() {}, schedule: (callback, delay) => { delays[Object.keys(delays).length] = delay; return 1; }, cancel() {} });
  bridge.request('agentInspect', { agentId: 'opencode' }).catch(() => {});
  bridge.request('conversationDelete', { conversationId: 'c' }).catch(() => {});
  bridge.request('conversationList').catch(() => {});
  assert.deepEqual(Object.values(delays), [35000, 50000, 15000]);
});

test('turn evidence operations are allowed with their own budgets; agent-only operations are not', async () => {
  const delays = [];
  const bridge = new AgentPanelBridge({ send() {}, schedule: (fn, delay) => { delays.push(delay); return delays.length; }, cancel() {}, maxPending: 64 });
  for (const operation of ['conversationArtifactRead', 'conversationRecoveryReview', 'conversationRecoveryTerminate', 'conversationRecoveryFork']) void bridge.request(operation, { conversationId: 'chat' }).catch(() => {});
  assert.deepEqual(delays, [12000, 17000, 17000, 12000]);
  for (const operation of ['agentActionBegin', 'agentActionReport', 'agentDeliveryReport', 'agentTaskAssess', 'agentRecordingAuthorize']) await assert.rejects(bridge.request(operation, {}), /Unsupported/);
  bridge.disconnect('test end');
});

test('artifact chunks must name the requested artifact, offset and a sha256 digest', async () => {
  const digest = `sha256:${'a'.repeat(64)}`;
  for (const [result, valid] of [
    [{ artifactId: 'art', offset: 0, data: 'AA==', total: 1, digest }, true],
    [{ artifactId: 'other', offset: 0, data: 'AA==', total: 1, digest }, false],
    [{ artifactId: 'art', offset: 4, data: 'AA==', total: 1, digest }, false],
    [{ artifactId: 'art', offset: 0, data: 7, total: 1, digest }, false],
    [{ artifactId: 'art', offset: 0, data: 'AA==', total: 1, digest: 'md5:00' }, false],
    [{ artifactId: 'art', offset: 0, data: 'AA==', digest }, false],
  ]) {
    const { bridge, messages } = setup();
    const reply = bridge.request('conversationArtifactRead', { conversationId: 'chat', artifactId: 'art', offset: 0, length: 393216 });
    bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, result: { schemaVersion: 1, ...result } });
    if (valid) assert.equal((await reply).artifactId, 'art'); else await assert.rejects(reply, /Malformed/);
  }
});

test('recovery replies are validated: review process state, terminate confirmation, fork identity', async () => {
  const cases = [
    ['conversationRecoveryReview', { process: { state: 'alive' } }, true],
    ['conversationRecoveryReview', { process: { state: 'gone' } }, false],
    ['conversationRecoveryReview', { conversation: { id: 'chat', phase: 'recoveryRequired' } }, false],
    ['conversationRecoveryTerminate', { confirmed: false }, true],
    ['conversationRecoveryTerminate', { confirmed: 'yes' }, false],
    ['conversationRecoveryFork', { conversation: { id: 'fork', phase: 'idle' }, forkedFrom: 'chat' }, true],
    ['conversationRecoveryFork', { conversation: { id: 'chat', phase: 'idle' }, forkedFrom: 'chat' }, false],
    ['conversationRecoveryFork', { conversation: { id: 'fork', phase: 'idle' }, forkedFrom: 'other' }, false],
  ];
  for (const [operation, result, valid] of cases) {
    const { bridge, messages } = setup();
    const reply = bridge.request(operation, { conversationId: 'chat' });
    bridge.receive({ method: 'agentPanelReply', requestId: messages[0].requestId, result: { schemaVersion: 1, ...result } });
    if (valid) await reply; else await assert.rejects(reply, /Malformed/);
  }
});

test('a timed-out read is not described as an unconfirmed conversation change', async () => {
  for (const [operation, expected] of [['conversationArtifactRead', /not replayed/], ['conversationRecoveryReview', /not replayed/], ['conversationRecoveryTerminate', /unconfirmed/]]) {
    const { bridge, timers } = setup();
    const reply = bridge.request(operation, { conversationId: 'chat' });
    [...timers.values()][0]();
    await assert.rejects(reply, expected);
  }
});
