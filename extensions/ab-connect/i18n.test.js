import { installI18n, loadMessages } from './test-i18n.js';
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync, readdirSync } from 'node:fs';
import { createPanel } from './sidepanel.js';

const dir = new URL('./', import.meta.url);
const read = name => readFileSync(new URL(name, dir), 'utf8');
const en = loadMessages('en'), zh = loadMessages('zh_CN');
const placeholderNames = entry => Object.keys(entry.placeholders || {}).map(name => name.toLowerCase()).sort();
const usedPlaceholders = message => [...new Set([...message.matchAll(/\$([A-Za-z0-9_@]+)\$/g)].map(m => m[1].toLowerCase()))].sort();
const cjk = /[一-鿿]/;

test('en and zh_CN define the same keys with the same placeholders', () => {
  assert.deepEqual(Object.keys(zh).sort(), Object.keys(en).sort());
  for (const key of Object.keys(en)) {
    assert.deepEqual(placeholderNames(zh[key]), placeholderNames(en[key]), key);
    for (const [name, value] of Object.entries(en[key].placeholders || {})) {
      const other = Object.entries(zh[key].placeholders).find(([n]) => n.toLowerCase() === name.toLowerCase())[1];
      assert.equal(other.content, value.content, `${key} $${name}$`);
    }
    for (const messages of [en, zh]) assert.deepEqual(usedPlaceholders(messages[key].message), placeholderNames(messages[key]), `${key} uses exactly its placeholders`);
  }
});

test('no message is empty and key names are valid for chrome.i18n', () => {
  for (const messages of [en, zh]) {
    for (const [key, entry] of Object.entries(messages)) {
      assert.match(key, /^[A-Za-z0-9_@]+$/);
      assert.equal(typeof entry.message, 'string', key);
      assert.ok(entry.message.trim(), `${key} is empty`);
    }
  }
});

// Every key is a literal in the source (t('…'), messageMap values, data-i18n*
// attributes, __MSG_…__ in the manifest), so a static scan finds them all.
const prefixes = 'panel|sidepanel|timeline|notify|entry|inspect|bg|popup|options|ext';
function referencedKeys() {
  const keys = new Map();
  const note = (key, file) => keys.set(key, file);
  for (const file of readdirSync(dir).filter(f => /\.(js|html|json)$/.test(f) && !f.endsWith('.test.js') && !f.startsWith('test-'))) {
    const text = read(file);
    if (file.endsWith('.js')) for (const m of text.matchAll(new RegExp(`['"\`]((?:${prefixes})_[a-z0-9_]+)['"\`]`, 'g'))) note(m[1], file);
    if (file.endsWith('.html')) for (const m of text.matchAll(/data-i18n(?:-title|-placeholder|-aria-label)?="([^"]+)"/g)) note(m[1], file);
    if (file === 'manifest.json') for (const m of text.matchAll(/__MSG_([A-Za-z0-9_@]+)__/g)) note(m[1], file);
  }
  return keys;
}

test('every key referenced in code, HTML or the manifest exists in en', () => {
  const missing = [...referencedKeys()].filter(([key]) => !en[key]).map(([key, file]) => `${key} (${file})`);
  assert.deepEqual(missing, []);
});

test('every en key is referenced somewhere', () => {
  const keys = referencedKeys();
  assert.deepEqual(Object.keys(en).filter(key => !keys.has(key)), []);
});

test('the manifest is localized with English as the default', () => {
  const manifest = JSON.parse(read('manifest.json'));
  assert.equal(manifest.default_locale, 'en');
  assert.match(manifest.description, /^__MSG_\w+__$/);
  assert.match(manifest.action.default_title, /^__MSG_\w+__$/);
  assert.match(manifest.commands['inspect-element'].description, /^__MSG_\w+__$/);
  assert.ok(zh.ext_description.message.length <= 132, 'Web Store description limit');
});

test('check and stop wording follows the panel design in both languages', () => {
  assert.equal(en.timeline_assessment_checks_passed.message, "Agent's browser checks passed");
  assert.equal(zh.timeline_assessment_checks_passed.message, 'agent 的浏览器检查已通过');
  assert.equal(en.panel_control_stopped.message, "This conversation's agent has stopped");
  assert.equal(zh.panel_control_stopped.message, '这个会话的 agent 已停止');
  for (const messages of [en, zh]) assert.ok(!Object.values(messages).some(entry => /\bVerified\b|you have control/i.test(entry.message)));
});

// zh_CN smoke test: the same small DOM fixture as sidepanel.test.js.
class Node {
  constructor(tag = 'div') {
    this.tag = tag; this.children = []; this.listeners = {}; this.dataset = {};
    this.value = ''; this.textContent = ''; this.hidden = false; this.disabled = false;
    this.scrollHeight = 100; this.clientHeight = 100; this.scrollTop = 0;
    this.classList = { toggle() {} };
  }
  append(...nodes) { for (const node of nodes) { node.parent = this; this.children.push(node); } }
  insertBefore(node, before) { if (node.parent) node.remove(); node.parent = this; const index = this.children.indexOf(before); this.children.splice(index < 0 ? this.children.length : index, 0, node); }
  replaceChildren(...nodes) { this.children = []; this.append(...nodes); }
  get firstElementChild() { return this.children[0]; }
  addEventListener(type, listener) { this.listeners[type] = listener; }
  setAttribute() {}
  querySelectorAll(tag) { return this.children.flatMap(node => [...(node.tag === tag ? [node] : []), ...node.querySelectorAll(tag)]); }
  remove() { this.parent.children = this.parent.children.filter(node => node !== this); }
}
const settle = async (rounds = 4) => { for (let i = 0; i < rounds; i++) await new Promise(resolve => setImmediate(resolve)); };
const text = node => [node.textContent, ...node.children.map(text)].filter(Boolean).join(' ');
const panelFixture = name => JSON.parse(readFileSync(new URL(`../../cli/src/agent_panel/fixtures/panel/${name}`, dir), 'utf8'));

async function panel({ support, conversation, events }) {
  const nodes = new Map();
  const runtime = {
    onMessage: { addListener() {} },
    async sendMessage(request) {
      if (request.type === 'ab-status') return { connected: true, agentPanel: support };
      switch (request.operation) {
        case 'discover': return { success: true, result: { agents: [] } };
        case 'conversationList': return { success: true, result: { conversations: [conversation] } };
        case 'conversationRead': return { success: true, result: { conversation, journal: { events: events.filter(e => e.sequence > request.params.after), cursor: events.at(-1)?.sequence ?? 0, more: false }, queuedMessages: [] } };
        case 'conversationMessageStatus': return { success: true, result: { message: { known: false } } };
        default: return { success: false, error: 'none' };
      }
    },
  };
  const document = { createElement: tag => new Node(tag), createTextNode: value => Object.assign(new Node('#text'), { textContent: value }), getElementById(id) { if (!nodes.has(id)) nodes.set(id, new Node()); return nodes.get(id); } };
  const instance = createPanel({ document, runtime, storage: { local: { async get() { return { ab_panel_conversation: conversation.id }; }, async set() {} } }, schedule() {}, cancel() {} });
  await instance.start(); await settle();
  return { panel: instance, nodes };
}

test('zh_CN: the side panel turn card renders in Chinese', async t => {
  installI18n('zh_CN'); t.after(() => installI18n('en'));
  const journal = panelFixture('journal-turns.json');
  const f = await panel({ support: journal.capabilities, conversation: journal.conversation, events: journal.events });
  const cards = [];
  const walk = node => { if (typeof node.className === 'string' && node.className.split(' ').includes('turn-card')) cards.push(node); node.children.forEach(walk); };
  walk(f.nodes.get('transcript'));
  assert.equal(cards.length, 2);
  const second = text(cards[1]);
  assert.ok(second.includes(zh.timeline_outcome_unknown.message), second);
  assert.ok(second.includes(zh.panel_empty_answer.message), second);
  assert.doesNotMatch(second, /Outcome unknown|The agent returned no answer text/);
  f.panel.close();
});

test('zh_CN: an approval card offers Chinese choices', async t => {
  installI18n('zh_CN'); t.after(() => installI18n('en'));
  const conversation = { id: 'a', title: 'a', phase: 'waitingInput', queued: 0, queuePaused: false, model: '' };
  const params = { serverName: 'chrome_use', mode: 'form', message: 'Allow browser navigation?', _meta: { codex_approval_kind: 'mcp_tool_call', tool_params: { url: 'http://127.0.0.1/fixture' }, tool_description: 'Navigate' }, requestedSchema: { type: 'object', properties: {} } };
  const f = await panel({ support: { schemaVersion: 1, mode: 'full', modelInspection: true }, conversation, events: [{ sequence: 1, kind: 'request', data: { requestId: 1, method: 'mcpServer/elicitation/request', params } }] });
  const labels = f.nodes.get('requests').querySelectorAll('button').map(button => button.textContent);
  assert.ok(labels.includes(zh.panel_allow_once.message), labels.join(', '));
  assert.ok(!labels.includes('Allow once'));
  f.panel.close();
});

// The options page is a module that runs against `document` on import; give it
// elements parsed from options.html so applyI18n and the status line both run.
test('zh_CN: the options page renders in Chinese', async () => {
  const elements = [];
  for (const m of read('options.html').matchAll(/<([a-z0-9]+)\b([^>]*)>/g)) {
    const attributes = Object.fromEntries([...m[2].matchAll(/([\w-]+)="([^"]*)"/g)].map(a => [a[1], a[2]]));
    elements.push({ attributes, textContent: '', value: '', style: {}, dataset: {}, classList: { add() {}, remove() {} }, addEventListener() {}, append() {}, replaceChildren() {},
      getAttribute(name) { return this.attributes[name] ?? null; }, setAttribute(name, value) { this.attributes[name] = value; } });
  }
  const byId = new Map(elements.filter(e => e.attributes.id).map(e => [e.attributes.id, e]));
  const document = {
    documentElement: elements.find(e => e.attributes.lang !== undefined),
    getElementById: id => byId.get(id) ?? elements[elements.push({ attributes: { id }, textContent: '', style: {}, classList: { add() {}, remove() {} }, addEventListener() {}, append() {}, replaceChildren() {} }) - 1],
    querySelectorAll: selector => { const name = /^\[([\w-]+)\]$/.exec(selector)?.[1]; return name ? elements.filter(e => name in e.attributes) : []; },
    createElement: () => ({ style: {}, classList: { add() {} }, append() {}, addEventListener() {} }),
  };
  const saved = { document: globalThis.document, chrome: globalThis.chrome, setTimeout: globalThis.setTimeout };
  const target = { chrome: {
    storage: { sync: { get(defaults, callback) { callback(defaults); }, set() {} }, local: { get(_key, callback) { callback({}); }, set() {} } },
    runtime: { getManifest: () => ({ version: '0.5.59' }), sendMessage(request, callback) { callback(request.type === 'ab-status' ? { connected: true, tabCount: 2 } : { success: true, result: { claims: [], bindings: [] } }); } },
    commands: { getAll(callback) { callback([{ name: 'inspect-element', shortcut: 'Alt+Shift+C' }]); } },
  } };
  installI18n('zh_CN', target);
  Object.assign(globalThis, { document, chrome: target.chrome, setTimeout: () => 0 });
  try {
    await import('./options.js');
    await settle();
    const translated = elements.filter(e => e.attributes['data-i18n']);
    assert.ok(translated.length > 10);
    // Some of these are rewritten by options.js afterwards (status, bindings),
    // so check that none is left showing its English text.
    for (const element of translated) {
      const key = element.attributes['data-i18n'];
      if (zh[key].message !== en[key].message) assert.notEqual(element.textContent, en[key].message, key);
    }
    assert.ok(translated.some(element => cjk.test(element.textContent)));
    assert.equal(byId.get('statusLabel').textContent, zh.options_status_connected.message);
    assert.match(byId.get('tabPill').textContent, /2/);
    assert.ok(cjk.test(byId.get('tabPill').textContent));
    assert.equal(document.documentElement.lang, 'zh-CN');
  } finally {
    Object.assign(globalThis, saved);
  }
});
