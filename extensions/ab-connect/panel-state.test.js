import assert from 'node:assert/strict';
import test from 'node:test';
import { PanelTimeline } from './panel-state.js';
const event = (sequence, kind, data) => ({ sequence, kind, data, at: sequence });

test('queue edits and removals keep history from looking like executed original messages', () => {
  const timeline = new PanelTimeline();
  timeline.apply([event(1,'user',{messageId:'old',text:'old text'}),event(2,'queueEdited',{messageId:'old',replacementId:'new',text:'edited text'})],2);
  assert.equal(timeline.rows.length,1); assert.equal(timeline.rows[0].text,'edited text');
  assert.equal(timeline.rows[0].messageId,'new'); assert.equal(timeline.rows[0].queueStatus,'Edited before sending');
  timeline.apply([event(3,'queueRemoved',{messageId:'new'})],3);
  assert.equal(timeline.rows[0].queueStatus,'Removed from queue');
});

test('reconnect replay cannot duplicate user messages or partial output', () => {
  const timeline = new PanelTimeline();
  const events = [event(1, 'user', { messageId: 'one', text: 'fixture' }), event(2, 'textDelta', { itemId: 'answer', text: 'partial' })];
  timeline.apply(events, 2); timeline.apply(events, 2);
  assert.equal(timeline.rows.length, 2); assert.equal(timeline.rows[1].text, 'partial');
});
test('completed chunks replace streamed text rather than duplicating the response', () => {
  const timeline = new PanelTimeline();
  timeline.apply([event(1, 'textDelta', { itemId: 'answer', text: 'partial' }), event(2, 'textFinal', { itemId: 'answer', text: 'full ', part: 0 }), event(3, 'textFinal', { itemId: 'answer', text: 'response', part: 1 })], 3);
  assert.equal(timeline.rows.length, 1); assert.equal(timeline.rows[0].text, 'full response');
});
test('history gaps and bad chunk order never look like a complete transcript', () => {
  const timeline = new PanelTimeline();
  assert.throws(() => timeline.apply([event(2, 'user', { text: 'fixture' })], 2), /gap/);
  assert.equal(timeline.cursor, 0);
  assert.throws(() => timeline.apply([event(1, 'textFinal', { itemId: 'answer', text: 'tail', part: 1 })], 1), /order/);
});
test('permission replies remain pending until a backend resolution or terminal event', () => {
  const timeline = new PanelTimeline();
  timeline.apply([event(1, 'request', { requestId: 7, method: 'fixture' }), event(2, 'replySent', { requestId: 7 })], 2);
  assert.equal(timeline.requests.get('7').sent, true);
  timeline.apply([event(3, 'requestResolved', { requestId: 7 })], 3); assert.equal(timeline.requests.size, 0);
});
test('a returned tool is not labeled successful when its outcome is unknown', () => {
  const timeline = new PanelTimeline();
  timeline.apply([event(1, 'tool', { completed: true, item: { id: 'one', name: 'click', output: 'unconfirmed' } })], 1);
  assert.equal(timeline.rows[0].completed, true); assert.equal(timeline.rows[0].failed, false); assert.equal(timeline.rows[0].text, 'unconfirmed');
});

test('a reused backend item id in another turn remains a separate response', () => {
  const timeline = new PanelTimeline();
  timeline.apply([event(1, 'textDelta', { messageId: 'one', itemId: 'same', text: 'first' }), event(2, 'textDelta', { messageId: 'two', itemId: 'same', text: 'second' })], 2);
  assert.equal(timeline.rows.length, 2); assert.equal(timeline.rows[0].text, 'first'); assert.equal(timeline.rows[1].text, 'second');
});

test('raw API error JSON becomes a readable note with the original kept as detail', () => {
  const timeline = new PanelTimeline();
  const raw = '{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"The \'x\' model is not supported."}}';
  timeline.apply([event(1,'terminal',{status:'failed',error:raw}),event(2,'exited',{error:'plain failure'})],2);
  assert.equal(timeline.rows[0].text,"The 'x' model is not supported."); assert.equal(timeline.rows[0].detail,raw);
  assert.equal(timeline.rows[1].text,'plain failure'); assert.equal(timeline.rows[1].detail,undefined);
});

test('commands show their command line and output; reasoning is not a tool row', () => {
  const timeline = new PanelTimeline();
  timeline.apply([
    event(1,'tool',{messageId:'m',completed:true,item:{id:'r',type:'reasoning',summary:[]}}),
    event(2,'tool',{messageId:'m',completed:true,item:{id:'c',type:'commandExecution',command:'ls',aggregatedOutput:'a.txt'}}),
    event(3,'tool',{messageId:'m',completed:true,item:{id:'t',type:'mcpToolCall',tool:'chrome_use_read',arguments:{tabId:'chrome-tab:7'},result:{content:[{type:'text',text:'page text'}]}}}),
    event(4,'tool',{messageId:'m',completed:true,item:{}}),
  ],4);
  assert.deepEqual(timeline.rows.map(r=>r.name),['Ran a command','Browser · read']); assert.deepEqual(timeline.rows.map(r=>r.hint),['ls','chrome-tab:7']);
  assert.equal(timeline.rows[0].text,'$ ls\n\na.txt');
  assert.match(timeline.rows[1].text,/chrome-tab:7[\s\S]*page text/);
});

// Fixtures live outside the extension folder so they never ship to the store.
import { readFileSync } from 'node:fs';
import { assessmentLabel, outcomeLabel, stepVerdict, stepActions, actingPages } from './panel-state.js';
const fixture = name => JSON.parse(readFileSync(new URL(`../../cli/src/agent_panel/fixtures/panel/${name}`, import.meta.url), 'utf8'));
const turnsJournal = fixture('journal-turns.json'), legacyJournal = fixture('journal-v1.5.214.json'), oldestJournal = fixture('journal-oldest.json');
const replay = events => { const timeline = new PanelTimeline(); timeline.apply(events, events.at(-1)?.sequence ?? 0); return timeline; };
const rowsOf = (timeline, turn) => turn.steps.map(key => timeline.items.get(key));

test('one turn per user message, with evidence joined by the messageId it was journaled with', () => {
  const timeline = replay(turnsJournal.events);
  assert.equal(timeline.turnOrder.length, 2);
  const [first, second] = timeline.turnOrder.map(id => timeline.turns.get(id));
  assert.equal(first.state, 'ended'); assert.equal(first.outcome, 'completed'); assert.equal(outcomeLabel(first), 'Completed');
  assert.equal(second.outcome, 'unknown'); assert.equal(second.continuation, 'afterStop'); assert.equal(second.emptyAnswer, true);
  assert.equal(first.actions.size, 7); assert.equal(second.actions.size, 2);
  assert.equal(first.roles.get(`text:${first.messageId}:msg_f`), 'final'); assert.equal(first.roles.get(`text:${first.messageId}:msg_c`), 'commentary');
  assert.equal(assessmentLabel(first), 'Some checks passed'); assert.equal(assessmentLabel(second), 'Not checked');
  assert.equal(second.batchInterrupted.unresolved[0].executed, 'unknown');
  assert.equal(second.stalls.length, 1); assert.equal(second.compactions.length, 1);
  assert.equal(timeline.control, 'unknown');
});

test('evidence before or after the tool event links identically', () => {
  const timeline = replay(turnsJournal.events);
  const turn = timeline.turns.get(timeline.turnOrder[0]);
  const verdicts = Object.fromEntries(rowsOf(timeline, turn).map(row => [row.toolCallId, stepVerdict(timeline, turn, row)]));
  // call_open: evidence first; call_fill: tool event first.
  assert.deepEqual(verdicts, { call_open: 'Not checked', call_fill: 'Confirmed', call_click: 'Failed', call_shot: 'Confirmed', call_dl: 'Confirmed', call_read: 'No browser record' });
  const fill = turnsJournal.events.filter(e => e.data.actionId === 'a2222222-2222-4222-8222-222222222222' || e.data.toolCallId === 'call_fill');
  const reversed = [...fill].reverse().map((e, i) => ({ ...e, sequence: i + 1 }));
  const forward = fill.map((e, i) => ({ ...e, sequence: i + 1 }));
  const a = replay(forward), b = replay(reversed);
  assert.equal(a.actions.get('a2222222-2222-4222-8222-222222222222').linkedStep, b.actions.get('a2222222-2222-4222-8222-222222222222').linkedStep);
  assert.equal(stepActions(a, a.items.values().find(r => r.type === 'tool')).length, 1);
  assert.equal(stepActions(b, b.items.values().find(r => r.type === 'tool')).length, 1);
});

test('unlinked evidence stays under its turn; a claimed id cannot be claimed by a second step', () => {
  const timeline = replay(turnsJournal.events);
  const turn = timeline.turns.get(timeline.turnOrder[0]);
  const unlinked = [...turn.actions.values()].filter(action => !action.linkedStep).map(action => action.actionId);
  assert.deepEqual(unlinked, ['a6666666-6666-4666-8666-666666666666', 'a9999999-9999-4999-8999-999999999999']);
  const extra = { sequence: turnsJournal.events.length + 1, kind: 'tool', data: { messageId: turn.messageId, completed: true, toolCallId: 'dup', actionIds: ['a2222222-2222-4222-8222-222222222222'], item: { id: 'dup', type: 'mcpToolCall', tool: 'chrome_use_fill' } } };
  timeline.apply([extra], extra.sequence);
  const dup = timeline.items.get(`tool:${turn.messageId}:dup`);
  assert.deepEqual(dup.duplicateClaims, ['a2222222-2222-4222-8222-222222222222']);
  assert.equal(stepVerdict(timeline, turn, dup), 'No browser record');
});

test('an OpenCode wrapper links every action id it returned', () => {
  const timeline = replay(turnsJournal.events);
  const turn = timeline.turns.get(timeline.turnOrder[1]);
  const wrapper = rowsOf(timeline, turn).find(row => row.toolCallId === 'exec_1');
  assert.equal(wrapper.browser, true);
  assert.deepEqual(stepActions(timeline, wrapper).map(action => action.tool), ['chrome_use_read', 'chrome_use_click']);
  // A read carries no verdict and the click was uncertain: not "Confirmed", and
  // the wrapper's own completed status is never read as success.
  assert.equal(stepVerdict(timeline, turn, wrapper), 'Not checked');
});

test('late reports join their own turn and are marked late', () => {
  const timeline = replay(turnsJournal.events);
  const late = timeline.actions.get('a9999999-9999-4999-8999-999999999999');
  assert.equal(late.messageId, timeline.turnOrder[0]); assert.equal(late.late, true); assert.equal(late.afterStop, true); assert.equal(late.unreported, true);
  assert.equal(timeline.turns.get(timeline.turnOrder[1]).actions.has(late.actionId), false);
});

test('unbound reports never join a turn', () => {
  const timeline = replay(turnsJournal.events);
  assert.equal(timeline.unbound.length, 1);
  for (const turn of timeline.turns.values()) for (const action of turn.actions.values()) assert.ok(action.actionId);
  assert.equal([...timeline.turns.values()].some(turn => [...turn.actions.values()].some(action => action.tool === 'chrome_use_click' && !action.target)), false);
});

test('acting pages come only from actionReply targets; redirects say where the browser landed', () => {
  const timeline = replay(turnsJournal.events);
  const first = actingPages(timeline.turns.get(timeline.turnOrder[0]));
  assert.deepEqual(first.actedOn.map(t => t.url), ['https://shop.example/checkout/step-2']);
  assert.deepEqual(first.pointedAt, []);
  assert.deepEqual(first.redirects, [{ asked: 'https://shop.example/checkout', landed: 'https://shop.example/checkout/step-2' }]);
  const second = actingPages(timeline.turns.get(timeline.turnOrder[1]));
  assert.equal(second.actedOn[0].attached, false);
  // A page the user only switched to is in no event, so it cannot appear.
  assert.equal(JSON.stringify(second).includes('chrome-tab:9999'), false);
});

test('artifacts carry their own state, type check and delivery separately', () => {
  const timeline = replay(turnsJournal.events);
  const turn = timeline.turns.get(timeline.turnOrder[0]);
  const [shot, download] = [...turn.artifacts.values()];
  assert.equal(shot.state, 'ready'); assert.equal(shot.kind, 'screenshot'); assert.match(shot.digest, /^sha256:[0-9a-f]{64}$/);
  assert.equal(download.typeMatches, false); assert.equal(download.state, 'ready'); assert.equal(download.delivery.state, 'failed');
});

test('request cards are keyed by revision; a new revision retires the old card', () => {
  const timeline = new PanelTimeline();
  timeline.apply([event(1, 'request', { requestId: 3, requestRevision: 1, generation: 2, method: 'can_use_tool', params: {} })], 1);
  assert.deepEqual([...timeline.requests.keys()], ['3#1']);
  timeline.apply([event(2, 'request', { requestId: 3, requestRevision: 2, generation: 3, method: 'can_use_tool', params: {} }), event(3, 'replySent', { requestId: 3 })], 3);
  assert.deepEqual([...timeline.requests.keys()], ['3#2']); assert.equal(timeline.requests.get('3#2').sent, true);
  timeline.apply([event(4, 'requestResolved', { requestId: 3 })], 4); assert.equal(timeline.requests.size, 0);
});

test('a v1.5.214 journal renders "not recorded" labels and never a verdict', () => {
  const timeline = replay(legacyJournal.events);
  assert.equal(timeline.turnOrder.length, 2);
  for (const id of timeline.turnOrder) {
    const turn = timeline.turns.get(id);
    assert.equal(turn.recorded, false); assert.equal(assessmentLabel(turn), 'Checks not recorded'); assert.equal(turn.legacy.grouping, false);
    for (const row of rowsOf(timeline, turn)) assert.ok([null, 'Page not recorded'].includes(stepVerdict(timeline, turn, row)));
    assert.equal(turn.artifacts.size, 0);
  }
  const first = timeline.turns.get(timeline.turnOrder[0]);
  assert.deepEqual(rowsOf(timeline, first).map(row => stepVerdict(timeline, first, row)), ['Page not recorded', 'Page not recorded', null]);
  assert.equal(outcomeLabel(first), 'Completed'); assert.equal(outcomeLabel(timeline.turns.get(timeline.turnOrder[1])), 'Interrupted');
  // A path in tool text never becomes a result card.
  assert.equal(timeline.artifacts.size, 0);
});

test('the oldest journals group events without a messageId under the preceding message', () => {
  const timeline = replay(oldestJournal.events);
  assert.equal(timeline.turnOrder.length, 1);
  const turn = timeline.turns.get(timeline.turnOrder[0]);
  assert.equal(turn.legacy.grouping, true); assert.equal(turn.steps.length, 1); assert.equal(turn.texts.length, 1);
});

test('replay from 0 and incremental replay produce deep-equal turns', () => {
  for (const journal of [turnsJournal, legacyJournal, oldestJournal]) {
    const whole = replay(journal.events);
    const incremental = new PanelTimeline();
    for (let i = 0; i < journal.events.length; i += 3) {
      const page = journal.events.slice(0, i + 3);
      incremental.apply(page, page.at(-1).sequence); // overlapping pages are ignored by cursor
    }
    assert.deepEqual(incremental, whole);
  }
});

test('unknown kinds advance the cursor and leave turns unchanged', () => {
  const timeline = replay(turnsJournal.events);
  const before = structuredClone([...timeline.turns]);
  const next = turnsJournal.events.length + 1;
  timeline.apply([event(next, 'someFutureKind', { v: 9, messageId: timeline.turnOrder[0] })], next);
  assert.equal(timeline.cursor, next); assert.deepEqual([...timeline.turns], before);
});

test('queue edits move the turn to the replacement id', () => {
  const timeline = new PanelTimeline();
  timeline.apply([event(1, 'user', { messageId: 'old', text: 'a' }), event(2, 'queueEdited', { messageId: 'old', replacementId: 'new', text: 'b' }), event(3, 'turnStarted', { v: 1, messageId: 'new', generation: 1 })], 3);
  assert.deepEqual(timeline.turnOrder, ['new']); assert.equal(timeline.turns.get('new').user.text, 'b'); assert.equal(timeline.turns.get('new').state, 'running');
  assert.equal(assessmentLabel(timeline.turns.get('new')), 'Assessment pending');
});
