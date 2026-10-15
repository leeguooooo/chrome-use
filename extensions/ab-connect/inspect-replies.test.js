import assert from 'node:assert/strict';
import test from 'node:test';
import { REPLY_TEXT, renderReplies, replyKey, replyNotice, validReplies } from './inspect-replies.js';
import { renderMarkdown } from './markdown.js';

class Node {
  constructor(tag) { this.tag = tag; this.children = []; this.attributes = {}; this.text = ''; this.className = ''; }
  set textContent(value) { this.children = []; this.text = value; }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren(...nodes) { this.children = nodes; this.text = ''; }
  setAttribute(name, value) { this.attributes[name] = value; }
}
const document = { createElement: tag => new Node(tag), createTextNode: text => Object.assign(new Node('#text'), { text }) };
const tags = node => [node.tag, ...node.children.flatMap(tags)];
const reply = (fields = {}) => ({ deliveryId: 'd1', to: 'chrome-use.inspect.x', channel: 'dm-a', seq: 8, from: 'chrome-use-91', at: '2026-10-15T00:00:00Z', text: 'Done', receivedAt: 1000, ...fields });

test('only replies with the promised shape are kept', () => {
  assert.deepEqual(validReplies(null), []);
  assert.deepEqual(validReplies({ replies: 'x' }), []);
  const good = reply();
  const kept = validReplies({ replies: [good, { ...good, seq: '8' }, { ...good, text: 5 }, { ...good, deliveryId: 3 }, null, { ...good, deliveryId: undefined }] });
  assert.equal(kept.length, 2);
  assert.equal(replyKey(good), 'dm-a#8');
});

test('reply text renders as Markdown nodes, never as HTML', () => {
  const list = new Node('div');
  const count = renderReplies(document, list, [reply({ text: '**fixed** <img src=x onerror=alert(1)>\n[link](javascript:alert(1))', truncated: true }), reply({ seq: 9, deliveryId: undefined, text: 'other' })], renderMarkdown, { showBatch: true });
  assert.equal(count, 2);
  const first = list.children[0];
  assert.match(first.textContent, /^Reply from chrome-use-91 /);
  assert.ok(tags(first).includes('strong'));
  assert.ok(!tags(first).includes('img') && !tags(first).includes('a'), 'no markup or unsafe link becomes an element');
  assert.ok(first.textContent.includes('<img src=x onerror=alert(1)>'));
  assert.ok(first.textContent.includes(REPLY_TEXT.truncated));
  assert.ok(!first.textContent.includes(REPLY_TEXT.unlinked));
  assert.ok(list.children[1].textContent.includes(REPLY_TEXT.unlinked));
});

test('a notification carries the sender and the first line only', () => {
  assert.deepEqual(replyNotice(reply({ text: '\n  First line\nsecret second line' })), { title: 'Reply from chrome-use-91', message: 'First line' });
  assert.equal(replyNotice(reply({ text: 'x'.repeat(300) })).message.length, 120);
});
