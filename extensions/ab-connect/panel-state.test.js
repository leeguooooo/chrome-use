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
