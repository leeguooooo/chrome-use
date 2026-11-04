import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import {
  SITE_PERMISSIONS_KEY, SITE_REQUESTS_KEY, DECIDED_TTL_MS, REQUEST_TTL_MS,
  addRequest, applyDecision, createSitePermissions, editSites, effectiveSettings, hostMatches,
  lockedSettings, managedPolicy, normalizeHost, normalizeStore, pruneRequests, requestStatus,
  sitePolicyReply, updateSettings,
} from './site-permissions.js';
import { approvalCard } from './approval-card.js';
import { requestSpec } from './site-approvals-view.js';

test('hosts normalise and match like the CLI: a domain covers its subdomains', () => {
  assert.equal(normalizeHost('https://WWW.Example.com:8443/a?b'), 'www.example.com');
  assert.equal(normalizeHost('*.example.com'), 'example.com');
  assert.equal(normalizeHost('  '), '');
  assert.ok(hostMatches('example.com', 'a.b.example.com'));
  assert.ok(!hostMatches('example.com', 'badexample.com'));
  assert.ok(hostMatches('*', 'x.test'));
});

test('a store from an older or broken build is made safe', () => {
  const s = normalizeStore({ allow: ['A.test', 'a.test', 3], settings: { ask: ['download', 'bogus'], sensitiveSites: 'yes' }, history: 'x' });
  assert.deepEqual(s.allow, ['a.test']);
  assert.deepEqual(s.settings, { ask: ['download'] });
  assert.deepEqual(s.history, []);
});

test('the managed policy keeps only well-formed values and locks what it sets', () => {
  const m = managedPolicy({ allowedDomains: ['Corp.test'], sensitiveSites: false, ask: ['purchase', 'x'], actionPolicy: { default: 'deny', allow: ['click', 1], evil: 1 }, disabledAgents: ['pi'], unknown: 1 });
  assert.deepEqual(m, { allowedDomains: ['corp.test'], sensitiveSites: false, ask: ['purchase'], actionPolicy: { default: 'deny', allow: ['click'] }, disabledAgents: ['pi'] });
  assert.deepEqual(lockedSettings(m), { sensitiveSites: true, ask: true, firstVisit: false });
  const eff = effectiveSettings({ settings: { sensitiveSites: true, firstVisit: ['navigation'] } }, m);
  assert.deepEqual(eff, { sensitiveSites: false, ask: ['purchase'], firstVisit: ['navigation'] });
  assert.throws(() => updateSettings({}, { sensitiveSites: true }, m), /managed/);
  assert.deepEqual(updateSettings({}, { firstVisit: ['click'] }, m).settings, { firstVisit: ['click'] });
});

test('defaults: sensitive list on, payment fields and purchases ask; password fields, downloads and first visits are opt-in', () => {
  assert.deepEqual(effectiveSettings({}, {}), { sensitiveSites: true, ask: ['payment_input', 'purchase'], firstVisit: [] });
  assert.deepEqual(normalizeStore({ settings: { ask: ['password_input', 'download', 'sensitive_input'] } }).settings.ask, ['password_input', 'download']);
});

test('the policy reply carries only what the person set, so the CLI can apply its own defaults', () => {
  const reply = sitePolicyReply({ allow: ['a.test'], settings: { sensitiveSites: false } }, { blockedDomains: ['b.test'] });
  assert.deepEqual(reply, { version: 1, user: { allow: ['a.test'], block: [], allowKinds: {}, sensitiveSites: false }, managed: { blockedDomains: ['b.test'] } });
});

test('requests: one pending per session, status, and expiry', () => {
  let r = addRequest([], { id: 's_1', host: 'Bank.test', category: 'click', reason: 'sensitive_site', session: 'a' }, 1000);
  r = addRequest(r, { id: 's_2', host: 'bank.test', session: 'a' }, 2000);
  assert.deepEqual(r.map(x => x.id), ['s_2'], 'a new request from the same session replaces the old one');
  r = addRequest(r, { id: 's_3', host: 'x.test', session: 'b' }, 2000);
  assert.equal(r.length, 2);
  assert.deepEqual(requestStatus(r, 's_2'), { known: true, decision: null });
  assert.deepEqual(requestStatus(r, 'nope'), { known: false, decision: null });
  assert.throws(() => addRequest([], { id: 'bad id!' }, 0), /bad id/);
  assert.equal(pruneRequests(r, 2000 + REQUEST_TTL_MS).length, 0);
});

test('decisions: always adds the site and unblocks it; each answer lands in the history once', () => {
  const requests = addRequest([], { id: 's_1', host: 'bank.test', category: 'click', reason: 'sensitive_site', description: 'd', action: 'click', session: 'a' }, 0);
  const out = applyDecision({ block: ['bank.test'] }, requests, 's_1', 'always', 10);
  assert.deepEqual(out.store.allow, ['bank.test']);
  assert.deepEqual(out.store.block, []);
  assert.equal(out.store.history.length, 1);
  assert.equal(out.store.history[0].decision, 'always');
  assert.deepEqual(requestStatus(out.requests, 's_1'), { known: true, decision: 'always' });
  assert.throws(() => applyDecision(out.store, out.requests, 's_1', 'once', 11), /already answered/);
  assert.equal(pruneRequests(out.requests, 10 + DECIDED_TTL_MS).length, 0);

  const once = applyDecision({}, requests, 's_1', 'once', 10);
  assert.deepEqual(once.store.allow, [], 'allow once never remembers the site');
  const cancel = applyDecision({}, requests, 's_1', 'cancel', 10);
  assert.equal(cancel.requests.length, 0);
  assert.equal(cancel.store.history[0].decision, 'withdrawn');
  assert.throws(() => applyDecision({}, requests, 's_1', 'yes', 10), /unknown decision/);
});

test('editing the lists moves a site between allowed and blocked', () => {
  let s = editSites({ allow: ['a.test'] }, { list: 'block', host: 'https://a.test/x' }, 1);
  assert.deepEqual([s.allow, s.block], [[], ['a.test']]);
  s = editSites(s, { list: 'block', host: 'a.test', remove: true }, 2);
  assert.deepEqual(s.block, []);
  assert.throws(() => editSites(s, { list: 'allow', host: '*' }, 3), /site/);
  assert.throws(() => editSites(s, { list: 'other', host: 'a.test' }, 3), /Unknown list/);
});

function fakeChrome(managed = {}) {
  const areas = { local: {}, session: {} };
  const area = name => ({
    async get(key) { return key == null ? { ...areas[name] } : { [key]: areas[name][key] }; },
    async set(obj) { Object.assign(areas[name], obj); },
  });
  const notifications = { created: [], cleared: [], create(id, opts) { this.created.push({ id, opts }); }, clear(id) { this.cleared.push(id); } };
  return {
    areas, notifications,
    storage: { local: area('local'), session: area('session'), managed: { async get() { return managed; } } },
  };
}

test('worker: a request filed by the daemon is answered from the panel and read back by the daemon', async () => {
  const chrome = fakeChrome();
  let clock = 1000;
  const sp = createSitePermissions({ storage: chrome.storage, notifications: chrome.notifications, t: (k, h) => `${k}:${h ?? ''}`, now: () => clock });
  await sp.approval({ op: 'request', id: 's_9', host: 'secure.bank.test', category: 'click', reason: 'sensitive_site', description: 'why', action: 'click', session: 'default' });
  assert.equal(chrome.notifications.created[0].id, 'ab-site-s_9');
  assert.deepEqual(await sp.approval({ op: 'status', id: 's_9' }), { known: true, decision: null });
  await assert.rejects(sp.onMessage({ op: 'decide', id: 's_9', decision: 'cancel' }), /Allow once/);
  clock += 5;
  await sp.onMessage({ op: 'decide', id: 's_9', decision: 'always' });
  assert.deepEqual(await sp.approval({ op: 'status', id: 's_9' }), { known: true, decision: 'always' });
  assert.deepEqual(chrome.notifications.cleared, ['ab-site-s_9']);
  const policy = await sp.policy();
  assert.deepEqual(policy.user.allow, ['secure.bank.test']);
  assert.equal(chrome.areas.local[SITE_PERMISSIONS_KEY].history[0].by, 'panel');
  assert.ok(Array.isArray(chrome.areas.session[SITE_REQUESTS_KEY]));
});

test('worker: the terminal prompt records "always", and a withdrawn request disappears', async () => {
  const chrome = fakeChrome({ blockedDomains: ['evil.test'], sensitiveSites: true });
  const sp = createSitePermissions({ storage: chrome.storage, notifications: chrome.notifications, t: k => k, now: () => 1 });
  await sp.approval({ op: 'request', id: 's_a', host: 'a.test', session: 's1' });
  await sp.approval({ op: 'decide', id: 's_a', decision: 'always' });
  assert.deepEqual((await sp.policy()).user.allow, ['a.test']);
  assert.equal(chrome.areas.local[SITE_PERMISSIONS_KEY].history[0].by, 'terminal');
  await sp.approval({ op: 'request', id: 's_b', host: 'b.test', session: 's1' });
  await sp.approval({ op: 'cancel', id: 's_b' });
  assert.deepEqual(await sp.approval({ op: 'status', id: 's_b' }), { known: false, decision: null });
  assert.deepEqual((await sp.policy()).managed, { blockedDomains: ['evil.test'], sensitiveSites: true });
  await assert.rejects(sp.onMessage({ op: 'settings', patch: { sensitiveSites: false } }), /managed/);
  await assert.rejects(sp.approval({ op: 'bogus' }), /unknown op/);
});

test('the approval card is generic: any choices, errors re-enable the buttons', async () => {
  const made = [];
  const doc = {
    createElement(tag) {
      const node = { tag, children: [], dataset: {}, listeners: {}, textContent: '', className: '', disabled: false,
        append(...n) { this.children.push(...n); }, addEventListener(type, fn) { this.listeners[type] = fn; } };
      made.push(node); return node;
    },
  };
  let fail = true;
  const chosen = [];
  const card = approvalCard(doc, { title: 'T', detail: 'D', facts: [['Why', 'x'], ['Empty', '']], choices: [{ id: 'a', label: 'A', primary: true }, { id: 'b', label: 'B' }] }, async id => { chosen.push(id); if (fail) throw new Error('nope'); });
  const buttons = made.filter(n => n.tag === 'button');
  assert.deepEqual(buttons.map(b => [b.textContent, b.dataset.choice, b.className]), [['A', 'a', 'primary'], ['B', 'b', '']]);
  await buttons[1].listeners.click();
  assert.deepEqual(chosen, ['b']);
  assert.ok(buttons.every(b => !b.disabled), 'a failed choice can be retried');
  assert.equal(card.children.at(-1).textContent, 'nope');
  fail = false;
  await buttons[0].listeners.click();
  assert.ok(buttons.every(b => b.disabled));
  assert.equal(made.filter(n => n.tag === 'p' && n.className === 'hint').length, 2, 'an empty fact is skipped');
});

test('the site request card offers the three choices', () => {
  const spec = requestSpec({ host: 'bank.test', reason: 'purchase', description: 'd', action: 'click', session: 's' }, k => k);
  assert.deepEqual(spec.choices.map(c => c.id), ['once', 'always', 'decline']);
  assert.equal(spec.facts[0][1], 'panel_site_reason_purchase');
});

test('the manifest declares the managed schema, and the schema covers every managed key the code reads', () => {
  const manifest = JSON.parse(readFileSync(new URL('./manifest.json', import.meta.url)));
  assert.equal(manifest.storage.managed_schema, 'managed-schema.json');
  const schema = JSON.parse(readFileSync(new URL('./managed-schema.json', import.meta.url)));
  assert.deepEqual(Object.keys(schema.properties).sort(), ['actionPolicy', 'allowedDomains', 'ask', 'blockedDomains', 'confirmDomains', 'disabledAgents', 'firstVisit', 'sensitiveSites']);
  const all = Object.fromEntries(Object.keys(schema.properties).map(k => [k, schema.properties[k].type === 'boolean' ? true : schema.properties[k].type === 'object' ? { deny: ['upload'] } : ['download']]));
  assert.deepEqual(Object.keys(managedPolicy(all)).sort(), Object.keys(schema.properties).sort());
});

test('always on an always-ask kind remembers the kind for that site, and removing the site forgets it (#846)', () => {
  const requests = addRequest([], { id: 's_k', host: 'signin.aliyun.com', category: 'typing', reason: 'payment_input', description: 'd', action: 'fill', session: 'a' }, 0);
  const out = applyDecision({}, requests, 's_k', 'always', 10);
  assert.deepEqual(out.store.allow, ['signin.aliyun.com']);
  assert.deepEqual(out.store.allowKinds, { 'signin.aliyun.com': ['payment_input'] });
  assert.deepEqual(sitePolicyReply(out.store, {}).user.allowKinds, { 'signin.aliyun.com': ['payment_input'] });

  const once = applyDecision({}, requests, 's_k', 'once', 10);
  assert.deepEqual(once.store.allowKinds, {}, 'allow once remembers nothing');
  const site = applyDecision({}, addRequest([], { id: 's_s', host: 'bank.test', reason: 'sensitive_site', session: 'a' }, 0), 's_s', 'always', 10);
  assert.deepEqual(site.store.allowKinds, {}, 'a site request does not allow any kind');

  const removed = editSites(out.store, { list: 'allow', host: 'signin.aliyun.com', remove: true }, 20);
  assert.deepEqual(removed.allowKinds, {});
  const blocked = editSites(out.store, { list: 'block', host: 'signin.aliyun.com' }, 20);
  assert.deepEqual(blocked.allowKinds, {});
  // Unknown kinds, and kinds for a host no longer approved, are dropped.
  assert.deepEqual(normalizeStore({ allow: ['a.test'], allowKinds: { 'a.test': ['payment_input', 'bogus'], 'b.test': ['purchase'] } }).allowKinds, { 'a.test': ['payment_input'] });
});
