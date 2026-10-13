import assert from 'node:assert/strict';
import test from 'node:test';
import { renderMarkdown } from './markdown.js';

class Node {
  constructor(tag) { this.tag = tag; this.children = []; this.attributes = {}; this.text = ''; this.className = ''; }
  set textContent(value) { this.children = []; this.text = value; }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren() { this.children = []; this.text = ''; }
  setAttribute(name, value) { this.attributes[name] = value; }
}
const document = { createElement: tag => new Node(tag), createTextNode: text => Object.assign(new Node('#text'), { text }) };
const render = source => { const root = new Node('div'); renderMarkdown(document, root, source); return root; };
const shape = node => node.tag === '#text' ? node.text : { [node.tag]: node.children.length ? node.children.map(shape) : node.text };

test('headings, emphasis, inline code and lists become elements', () => {
  const root = render('## 私有仓库 (3个)\n\n1. **api-gateway** - TypeScript\n2. `codex-review` 的配置\n\n- 📚 文档\n- 🔐 KYC');
  assert.deepEqual(root.children.map(n => n.tag), ['h4', 'ol', 'ul']);
  assert.deepEqual(shape(root.children[1].children[0]), { li: [{ strong: ['api-gateway'] }, ' - TypeScript'] });
  assert.deepEqual(shape(root.children[1].children[1]), { li: [{ code: 'codex-review' }, ' 的配置'] });
  assert.equal(root.children[2].children.length, 2);
});

test('identifiers with underscores are not italicised', () => {
  const root = render('Called mcp__chrome_use__chrome_use_open and _done_.');
  assert.deepEqual(shape(root.children[0]), { p: ['Called mcp__chrome_use__chrome_use_open and ', { em: ['done'] }, '.'] });
});

test('fenced code keeps its text literally, including markup', () => {
  const root = render('```html\n<script>alert(1)</script>\n**not bold**\n```');
  assert.equal(root.children[0].tag, 'pre');
  assert.equal(root.children[0].attributes['data-language'], 'html');
  assert.equal(root.children[0].children[0].text, '<script>alert(1)</script>\n**not bold**');
});

test('only http(s) links become anchors, and they open safely', () => {
  const root = render('[ok](https://example.com/a) [bad](javascript:alert(1)) https://x.com/p.');
  const anchors = root.children[0].children.filter(n => n.tag === 'a');
  assert.equal(anchors.length, 2);
  assert.equal(anchors[0].attributes.href, 'https://example.com/a');
  assert.equal(anchors[0].attributes.rel, 'noopener noreferrer');
  assert.equal(anchors[1].attributes.href, 'https://x.com/p');
  assert.match(root.textContent, /\[bad\]\(javascript:alert\(1\)\)/);
});

test('pipe tables, quotes and line breaks inside a paragraph', () => {
  const root = render('| a | b |\n|---|---|\n| 1 | **2** |\n\n> quoted\n\nline one\nline two');
  assert.equal(root.children[0].children[0].tag, 'table');
  assert.equal(root.children[1].tag, 'blockquote');
  assert.deepEqual(shape(root.children[2]), { p: ['line one', { br: '' }, 'line two'] });
});
