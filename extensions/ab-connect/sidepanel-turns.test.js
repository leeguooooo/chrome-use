import './test-i18n.js';
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { createPanel } from './sidepanel.js';

// The same deliberately small DOM fixture as sidepanel.test.js.
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
const settle = async (rounds = 4) => { for (let i = 0; i < rounds; i++) await new Promise(resolve => setImmediate(resolve)); };
// Digest checks run on WebCrypto's thread pool, so a fixed number of turns is
// not enough on a busy machine; wait for the condition instead.
const until = async (check, timeout = 2000) => { const end = Date.now() + timeout; while (!check()) { if (Date.now() > end) throw new Error('condition not met within ' + timeout + ' ms'); await new Promise(resolve => setTimeout(resolve, 5)); } };
const fixtureFile = name => JSON.parse(readFileSync(new URL(`../../cli/src/agent_panel/fixtures/panel/${name}`, import.meta.url), 'utf8'));
const turnsJournal = fixtureFile('journal-turns.json'), legacyJournal = fixtureFile('journal-v1.5.214.json');
const turnSupport = turnsJournal.capabilities;
const text = node => [node.textContent, ...node.children.map(text)].filter(Boolean).join(' ');
const all = (node, predicate) => [...(predicate(node) ? [node] : []), ...node.children.flatMap(child => all(child, predicate))];
const buttons = node => all(node, n => n.tag === 'button');
const byClass = (node, name) => all(node, n => typeof n.className === 'string' && n.className.split(' ').includes(name));

async function panel({ support = turnSupport, conversation = turnsJournal.conversation, events = turnsJournal.events, overrides = {}, create = createPanel, objectUrls, tabs } = {}) {
  const nodes = new Map(), calls = [];
  const runtime = {
    onMessage: { addListener() {} },
    async sendMessage(request) {
      if (request.type === 'ab-status') return { connected: true, agentPanel: support };
      calls.push(request);
      if (overrides[request.operation]) return overrides[request.operation](request.params);
      switch (request.operation) {
        case 'discover': return { success: true, result: { agents: [] } };
        case 'conversationList': return { success: true, result: { conversations: [conversation] } };
        case 'conversationRead': { const page = events.filter(e => e.sequence > request.params.after); return { success: true, result: { conversation, journal: { events: page, cursor: events.at(-1)?.sequence ?? 0, more: false }, queuedMessages: [] } }; }
        case 'conversationMessageStatus': return { success: true, result: { message: { known: false } } };
        case 'conversationAttachmentRead': return { success: false, error: 'none' };
        default: throw new Error(`Unexpected operation ${request.operation}`);
      }
    },
  };
  const document = { createElement: tag => new Node(tag), createTextNode: value => Object.assign(new Node('#text'), { textContent: value }), getElementById(id) { if (!nodes.has(id)) nodes.set(id, new Node()); return nodes.get(id); } };
  const instance = create({ document, runtime, storage: { local: { async get() { return { ab_panel_conversation: conversation.id }; }, async set() {} } }, schedule() {}, cancel() {}, tabs, ...(objectUrls ? { objectUrls } : {}) });
  await instance.start(); await settle();
  return { panel: instance, nodes, calls, transcript: nodes.get('transcript') };
}

test('turn evidence renders one card per message with acting pages from action replies only', async () => {
  const f = await panel();
  const cards = byClass(f.transcript, 'turn-card');
  assert.equal(cards.length, 2);
  const [first, second] = cards.map(text);
  assert.match(first, /Sent with: Cart · Shop · shop\.example/);
  assert.match(first, /Acted on: Checkout · shop\.example/);
  assert.match(first, /Asked for https:\/\/shop\.example\/checkout, landed on https:\/\/shop\.example\/checkout\/step-2/);
  assert.doesNotMatch(first, /Browser tools pointed at/);
  assert.match(first, /2 browser records not matched to a step/);
  assert.match(first, /reported after the request ended/);
  assert.match(second, /No record that it ran/);
  assert.match(second, /Outcome unknown/);
  assert.match(second, /The agent returned no answer text\./);
  f.panel.close();
});

test('a page the user only switched to never appears; the attached tab reads as the page you sent', async () => {
  const id = '11111111-2222-4333-8444-555555555555', act = 'c1111111-1111-4111-8111-111111111111', ptr = 'c2222222-2222-4222-8222-222222222222';
  const target = (tab, url, provenance) => ({ chromeTabId: tab, tabRef: `chrome-tab:${tab}`, targetId: `T${tab}`, url, title: null, provenance });
  const events = [
    { sequence: 1, kind: 'user', data: { messageId: id, text: 'go', page: { tabId: 5, url: 'https://a.example/', title: 'A page' } } },
    { sequence: 2, kind: 'turnStarted', data: { v: 1, messageId: id, generation: 1 } },
    { sequence: 3, kind: 'browserActionTarget', data: { v: 1, messageId: id, actionId: act, tool: 'chrome_use_click', toolClass: 'act', requested: null, observed: { before: null, after: target(5, 'https://a.example/', 'actionReply') } } },
    { sequence: 4, kind: 'browserActionTarget', data: { v: 1, messageId: id, actionId: ptr, tool: 'chrome_use_press', toolClass: 'act', requested: null, observed: { before: null, after: target(9, 'https://b.example/x', 'daemonState') } } },
  ];
  const tabs = { query: async () => [{ id: 77, url: 'https://c.example/', title: 'C page the user switched to' }], onActivated: { addListener() {} }, onUpdated: { addListener() {} } };
  const f = await panel({ tabs, events, conversation: { ...turnsJournal.conversation, phase: 'running', control: 'agent', activeMessageId: id } });
  assert.equal(f.nodes.get('pageTitle').textContent, 'C page the user switched to');
  const card = text(byClass(f.transcript, 'turn-card')[0]);
  assert.match(card, /Acted on: the page you sent/);
  assert.match(card, /Browser tools pointed at: b\.example\/x/);
  assert.doesNotMatch(card, /c\.example|switched/);
  assert.equal(f.nodes.get('control').textContent, 'Agent working');
  f.panel.close();
});

test('a folded group names its failed steps', async () => {
  const f = await panel();
  const group = byClass(byClass(f.transcript, 'turn-card')[0], 'tool-group')[0];
  assert.equal(group.children[0].textContent, '6 steps · 1 failed: Browser · click');
  assert.equal(group.open, false);
  const chips = byClass(group, 'verdict-chip').map(chip => chip.textContent);
  assert.deepEqual(chips, ['Not checked', 'Confirmed', 'Failed', 'Confirmed', 'Confirmed', 'No browser record']);
  f.panel.close();
});

test('assessment badge, requirement checks and the disclaimer; the wording never says Verified', async () => {
  const f = await panel();
  const [first, second] = byClass(f.transcript, 'turn-card');
  const badge = byClass(first, 'badge')[0];
  assert.equal(badge.textContent, 'Some checks passed');
  const body = text(byClass(first, 'assessment')[0]);
  assert.match(body, /Card number entered Checked/);
  assert.match(body, /field check: '#card has the typed value' on shop\.example\/checkout\/step-2/);
  assert.match(body, /Order paid Agent claimed/);
  assert.match(body, /These are the checks the agent chose; they do not prove the whole request was done\./);
  assert.equal(byClass(second, 'badge')[0].textContent, 'Not checked');
  for (const file of ['sidepanel.js', 'panel-state.js', 'sidepanel.html']) {
    const source = readFileSync(new URL(`./${file}`, import.meta.url), 'utf8').replace(/^\s*\/\/.*$/gm, '');
    assert.doesNotMatch(source, /['"`>][^'"`<]*\bVerified\b/, `${file} shows "Verified"`);
    assert.doesNotMatch(source, /you have control/i, file);
  }
  f.panel.close();
});

test('result cards keep capture, type, size and delivery as separate lines', async () => {
  const f = await panel({ support: { ...turnSupport, artifacts: undefined } });
  const results = byClass(f.transcript, 'result-card').map(text);
  assert.equal(results.length, 2);
  assert.match(results[1], /Download · receipt\.pdf/);
  assert.match(results[1], /File captured: yes/);
  assert.match(results[1], /Type matches \.pdf: no \(text\/html\)/);
  assert.match(results[1], /Not empty: yes/);
  assert.match(results[1], /Saved to your path: no \(destination folder is not writable\)/);
  // Without the artifacts capability no file is read.
  assert.equal(f.calls.some(call => call.operation === 'conversationArtifactRead'), false);
  f.panel.close();
});

test('a screenshot is shown only after its digest checks, and its object URL is revoked on switch', async () => {
  const png = Buffer.from(turnsJournal.pngBase64, 'base64');
  const created = [], revoked = [];
  const objectUrls = { create: blob => { created.push(blob); return `blob:test/${created.length}`; }, revoke: url => revoked.push(url) };
  const reads = [];
  const f = await panel({ objectUrls, overrides: { conversationArtifactRead: params => {
    reads.push(params);
    const digest = `sha256:${createHash('sha256').update(png).digest('hex')}`;
    return { success: true, result: { schemaVersion: 1, artifactId: params.artifactId, offset: params.offset, data: png.subarray(params.offset, params.offset + 40).toString('base64'), total: png.length, digest } };
  } } });
  await until(() => created.length === 1);
  assert.deepEqual(reads.map(read => read.offset), [0, 40]);
  assert.equal(reads.every(read => read.length === 384 * 1024), true);
  assert.equal(created.length, 1); assert.equal(created[0].size, png.length);
  const image = all(f.transcript, node => node.tag === 'img')[0];
  assert.equal(image.src, 'blob:test/1');
  f.panel.select(''); assert.deepEqual(revoked, ['blob:test/1']);
  f.panel.close();
});

test('bytes that do not match the digest are discarded and the card says the file changed', async () => {
  const png = Buffer.from(turnsJournal.pngBase64, 'base64');
  const tampered = Buffer.from(png); tampered[tampered.length - 1] ^= 1;
  const created = [];
  const f = await panel({ objectUrls: { create: blob => { created.push(blob); return 'blob:x'; }, revoke() {} }, overrides: { conversationArtifactRead: params => ({ success: true, result: { schemaVersion: 1, artifactId: params.artifactId, offset: params.offset, data: tampered.toString('base64'), total: tampered.length, digest: turnsJournal.events.find(e => e.kind === 'artifactReady').data.digest } }) } });
  await until(() => /Not shown: the file changed while it was read\./.test(text(byClass(f.transcript, 'result-card')[0])));
  assert.equal(created.length, 0);
  f.panel.close();
});

test('Allow is disabled while a stop is requested; decline stays available', async () => {
  const id = '22222222-2222-4333-8444-555555555555';
  const events = [
    { sequence: 1, kind: 'user', data: { messageId: id, text: 'go' } },
    { sequence: 2, kind: 'turnStarted', data: { v: 1, messageId: id, generation: 1 } },
    { sequence: 3, kind: 'request', data: { requestId: 4, requestRevision: 1, generation: 1, method: 'can_use_tool', params: { tool_name: 'Bash', input: { command: 'ls' } }, messageId: id } },
  ];
  for (const [control, allowDisabled] of [['stopRequested', true], ['agent', false]]) {
    const f = await panel({ events, conversation: { ...turnsJournal.conversation, phase: control === 'agent' ? 'waitingInput' : 'stopping', control, activeMessageId: id } });
    const [allow, decline] = buttons(f.nodes.get('requests'));
    assert.equal(allow.textContent, 'Allow once'); assert.equal(allow.disabled, allowDisabled);
    assert.equal(decline.textContent, 'Decline'); assert.equal(decline.disabled, false);
    if (control === 'stopRequested') assert.equal(f.nodes.get('control').textContent, 'Stop requested, waiting for confirmation');
    f.panel.close();
  }
});

test('recovery actions follow the review: terminate only when alive, continue only when exited, neither when unknown', async () => {
  for (const [state, expected] of [['alive', ['End the old agent process']], ['exited', ['Continue in a new conversation']], ['unknown', []]]) {
    const f = await panel({ overrides: { conversationRecoveryReview: () => ({ success: true, result: { schemaVersion: 1, process: { state, reason: state === 'unknown' ? 'the service restarted and cannot reopen the job' : null }, actions: [{ actionId: 'x', tool: 'chrome_use_click', possiblyExecuted: true, target: { url: 'https://shop.example/cart' } }] } }) } });
    const card = byClass(f.transcript, 'turn-card')[1];
    assert.deepEqual(buttons(byClass(card, 'recovery-block')[0]).map(b => b.textContent), ['Review what may have happened']);
    buttons(byClass(card, 'recovery-block')[0])[0].fire('click'); await settle();
    const block = byClass(card, 'recovery-block')[0];
    const labels = buttons(block).map(b => b.textContent).filter(label => label !== 'Review what may have happened');
    assert.deepEqual(labels, expected, state);
    assert.match(text(block), /click on shop\.example · may have taken effect/);
    if (state === 'unknown') assert.match(text(block), /not offered, because its state cannot be proven/);
    f.panel.close();
  }
});

test('forking selects the new conversation; a fork links back to the original', async () => {
  const original = turnsJournal.conversation, forkId = '99999999-0000-4000-8000-000000000001';
  let conversations = [original];
  const fork = { ...original, id: forkId, phase: 'idle', control: 'stopped', activeMessageId: null };
  const forkEvents = [{ sequence: 1, kind: 'recoveryContext', data: { v: 1, fromConversationId: original.id, fromMessageId: 'm', fromSequence: 9, outcome: 'unknown', possiblyExecuted: [], lastTargets: [], artifacts: [] } }];
  const f = await panel({ overrides: {
    conversationRecoveryReview: () => ({ success: true, result: { schemaVersion: 1, process: { state: 'exited' } } }),
    conversationRecoveryFork: () => { conversations = [fork, original]; return { success: true, result: { schemaVersion: 1, conversation: fork, forkedFrom: original.id } }; },
    conversationList: () => ({ success: true, result: { conversations } }),
    conversationRead: params => params.conversationId === forkId
      ? { success: true, result: { conversation: fork, journal: { events: forkEvents.filter(e => e.sequence > params.after), cursor: 1, more: false }, queuedMessages: [] } }
      : { success: true, result: { conversation: original, journal: { events: turnsJournal.events.filter(e => e.sequence > params.after), cursor: turnsJournal.events.length, more: false }, queuedMessages: [] } },
  } });
  const card = () => byClass(f.transcript, 'turn-card')[1];
  buttons(byClass(card(), 'recovery-block')[0])[0].fire('click'); await settle();
  buttons(byClass(card(), 'recovery-block')[0]).find(b => b.textContent === 'Continue in a new conversation').fire('click'); await settle(8);
  assert.equal(f.nodes.get('conversation').value, forkId);
  assert.match(text(f.transcript), /continues one whose last request ended with an unknown outcome/);
  assert.ok(buttons(f.transcript).some(b => b.textContent === 'Open the earlier conversation'));
  f.panel.close();
});

test('the record toggle sends record: true for one message, only when the host supports recording', async () => {
  for (const recording of [1, undefined]) {
    const sent = [];
    const f = await panel({ support: { ...turnSupport, recording }, conversation: { ...turnsJournal.conversation, phase: 'idle', control: 'stopped' }, events: [], overrides: { conversationSend: params => { sent.push(params); return { success: true, result: { schemaVersion: 1, conversation: { ...turnsJournal.conversation, phase: 'running' }, message: { id: params.messageId, accepted: true, replayed: false } } }; } } });
    assert.equal(f.nodes.get('recordTaskLabel').hidden, !recording);
    f.nodes.get('recordTask').checked = true; f.nodes.get('message').value = 'record it';
    await f.panel.send();
    assert.equal(sent[0].record, recording ? true : undefined);
    if (recording) assert.equal(f.nodes.get('recordTask').checked, false);
    f.panel.close();
  }
});

test('without capability flags the panel keeps the legacy flat view', async () => {
  const f = await panel({ support: legacyJournal.capabilities, conversation: legacyJournal.conversation, events: legacyJournal.events });
  assert.equal(byClass(f.transcript, 'turn-card').length, 0);
  assert.ok(byClass(f.transcript, 'tool-group').length > 0);
  assert.equal(f.nodes.get('control').hidden, true);
  assert.equal(f.nodes.get('recordTaskLabel').hidden, true);
  assert.equal(f.calls.some(call => /Artifact|Recovery/.test(call.operation)), false);
  f.panel.close();
});

test('a v1.5.214 journal in the turn view says what was not recorded and never shows a verdict', async () => {
  const f = await panel({ conversation: legacyJournal.conversation, events: legacyJournal.events });
  const cards = byClass(f.transcript, 'turn-card');
  assert.equal(cards.length, 2);
  const chips = cards.flatMap(card => byClass(card, 'verdict-chip')).filter(chip => !chip.hidden).map(chip => chip.textContent);
  assert.deepEqual(chips, ['Page not recorded', 'Page not recorded']);
  assert.deepEqual(cards.map(card => byClass(card, 'badge')[0].textContent), ['Checks not recorded', 'Checks not recorded']);
  assert.match(text(cards[0]), /Pages acted on: not recorded/);
  assert.equal(byClass(f.transcript, 'result-card').length, 0);
  f.panel.close();
});

test('the v1.5.214 panel renders a journal written by this build without [object Object] or errors', async () => {
  const { createPanel: oldPanel } = await import('../../cli/src/agent_panel/fixtures/panel/v1.5.214/sidepanel.js');
  const masked = turnsJournal.events;
  const request = { sequence: masked.length + 1, kind: 'request', data: { requestId: 9, requestRevision: 1, generation: 3, method: 'can_use_tool', params: { tool_name: 'chrome_use_fill', input: { selector: '#pw', value: '[redacted secret, 12 chars]' } }, redactions: [{ path: 'params.input.value', kind: 'secret', length: 12 }], privacyClass: 'masked' } };
  const responded = [];
  const f = await panel({ create: oldPanel, events: [...masked, request], conversation: { ...turnsJournal.conversation, phase: 'waitingInput' }, overrides: { conversationRespond: params => { responded.push(params); return { success: true, result: { schemaVersion: 1, conversation: turnsJournal.conversation, response: { dispatched: true, confirmed: false } } }; } } });
  assert.doesNotMatch(text(f.transcript), /\[object Object\]/);
  assert.ok(f.transcript.children.length > 2);
  const allow = buttons(f.nodes.get('requests')).find(b => b.textContent === 'Allow once');
  allow.fire('click'); await settle();
  // The legacy echo is exactly the frozen projection the host journaled.
  assert.deepEqual(responded[0].result, { behavior: 'allow', updatedInput: request.data.params.input });
  f.panel.close();
});
