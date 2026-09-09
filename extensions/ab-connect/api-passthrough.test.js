import { strict as assert } from 'node:assert';
import test from 'node:test';
import {
  ALLOWED,
  POLICY_VERSION,
  executeCall,
  mutationTargets,
  policySummary,
  validateCall,
} from './api-passthrough.js';

function env({ owned = [], groups = {}, windows = {}, api = {} } = {}) {
  return {
    api,
    isOwned: (id) => owned.includes(id),
    tabsInGroup: async (gid) => groups[gid] || [],
    tabsInWindow: async (wid) => windows[wid] || [],
  };
}

test('namespaces off the list are refused by name, with the list', () => {
  for (const ns of ['debugger', 'identity', 'storage', 'nativeMessaging', 'runtime', 'management', 'scripting']) {
    const v = validateCall({ namespace: ns, method: 'anything' });
    assert.equal(v.ok, false);
    assert.match(v.error, new RegExp(`'${ns}' is not allowed`));
    assert.match(v.error, /Allowed: tabs, tabGroups, windows, downloads, webNavigation/);
  }
});

test('methods off the list are refused with the allowed set for that namespace', () => {
  const v = validateCall({ namespace: 'windows', method: 'remove', args: [1] });
  assert.equal(v.ok, false);
  assert.match(v.error, /windows\.remove is not allowed/);
  assert.match(v.error, /Allowed on windows: get, getAll, getCurrent, getLastFocused, update/);
  assert.equal(validateCall({ namespace: 'windows', method: 'create' }).ok, false);
  assert.equal(validateCall({ namespace: 'tabs', method: 'executeScript' }).ok, false);
  assert.equal(validateCall({ namespace: 'tabs', method: 'highlight' }).ok, false);
});

test('read-only calls validate as non-mutating', () => {
  const v = validateCall({ namespace: 'tabs', method: 'query', args: [{ active: true }] });
  assert.deepEqual(v, {
    ok: true,
    namespace: 'tabs',
    method: 'query',
    args: [{ active: true }],
    mutates: false,
  });
  assert.equal(validateCall({ namespace: 'tabGroups', method: 'query' }).mutates, false);
  assert.equal(validateCall({ namespace: 'webNavigation', method: 'getAllFrames', args: [{ tabId: 3 }] }).mutates, false);
});

test('argument shape is enforced before anything reaches Chrome', () => {
  assert.match(validateCall({ namespace: 'tabs', method: 'query', args: { active: true } }).error, /must be an array/);
  assert.match(validateCall({ namespace: 'tabs', method: 'query', args: [1, 2, 3, 4, 5] }).error, /at most 4/);
  assert.match(validateCall({ namespace: 'tabs', method: 'query', args: [() => 1] }).error, /JSON values/);
  assert.match(validateCall({ namespace: 'tabs', method: 'query', args: ['x'.repeat(70 * 1024)] }).error, /exceed/);
  assert.match(validateCall({}).error, /required/);
});

test('every mutating tabs.* call resolves to the tab ids it touches', async () => {
  const lookups = env();
  assert.deepEqual(await mutationTargets(validateCall({ namespace: 'tabs', method: 'remove', args: [7] }), lookups), { tabIds: [7] });
  assert.deepEqual(await mutationTargets(validateCall({ namespace: 'tabs', method: 'remove', args: [[7, 8]] }), lookups), { tabIds: [7, 8] });
  assert.deepEqual(await mutationTargets(validateCall({ namespace: 'tabs', method: 'group', args: [{ tabIds: [7, 9] }] }), lookups), { tabIds: [7, 9] });
  // No id == no way to know the scope == refused, not "allowed by default".
  assert.match((await mutationTargets(validateCall({ namespace: 'tabs', method: 'update', args: [{ url: 'x' }] }), lookups)).error, /tab id/);
  assert.match((await mutationTargets(validateCall({ namespace: 'tabs', method: 'group', args: [{}] }), lookups)).error, /tabIds/);
});

test('group and window mutations expand to the tabs inside them', async () => {
  const lookups = env({ groups: { 5: [1, 2] }, windows: { 9: [3] } });
  assert.deepEqual(await mutationTargets(validateCall({ namespace: 'tabGroups', method: 'update', args: [5, { title: 'x' }] }), lookups), { tabIds: [1, 2] });
  assert.deepEqual(await mutationTargets(validateCall({ namespace: 'windows', method: 'update', args: [9, { focused: true }] }), lookups), { tabIds: [3] });
  assert.match((await mutationTargets(validateCall({ namespace: 'tabGroups', method: 'update', args: [6, {}] }), lookups)).error, /no tabs/);
});

test('downloads mutations need an explicit id; erase never takes an open query', async () => {
  const lookups = env();
  assert.deepEqual(await mutationTargets(validateCall({ namespace: 'downloads', method: 'cancel', args: [12] }), lookups), { tabIds: [] });
  assert.match((await mutationTargets(validateCall({ namespace: 'downloads', method: 'erase', args: [{ state: 'complete' }] }), lookups)).error, /`\{ id \}` is required/);
  assert.deepEqual(await mutationTargets(validateCall({ namespace: 'downloads', method: 'erase', args: [{ id: 12 }] }), lookups), { tabIds: [] });
});

test('a mutation on a tab the relay does not own is refused, naming the tab', async () => {
  const removed = [];
  const e = env({ owned: [1], api: { tabs: { remove: async (id) => removed.push(id) } } });
  await assert.rejects(
    executeCall({ namespace: 'tabs', method: 'remove', args: [2] }, e),
    /tabs\.remove refused — tab 2 is not owned/
  );
  await assert.rejects(
    executeCall({ namespace: 'tabs', method: 'remove', args: [[1, 2, 3]] }, e),
    /tabs 2, 3 are not owned/
  );
  assert.deepEqual(removed, [], 'nothing reached Chrome');
  assert.deepEqual(await executeCall({ namespace: 'tabs', method: 'remove', args: [1] }, e), { result: 1 });
});

test('read-only calls run without an ownership check and return the API result', async () => {
  const e = env({ owned: [], api: { tabs: { query: async (q) => [{ id: 4, ...q }] } } });
  assert.deepEqual(await executeCall({ namespace: 'tabs', method: 'query', args: [{ active: true }] }, e), {
    result: [{ id: 4, active: true }],
  });
});

test('a Chrome rejection is passed through with the method as prefix; policy errors are not', async () => {
  const e = env({ owned: [1], api: { tabs: { update: async () => { throw new Error('No tab with id: 1.'); } } } });
  await assert.rejects(executeCall({ namespace: 'tabs', method: 'update', args: [1, { url: 'x' }] }, e), /^Error: tabs\.update: No tab with id: 1\./);
  await assert.rejects(executeCall({ namespace: 'debugger', method: 'attach' }, e), /^Error: call: namespace 'debugger'/);
});

test('an API missing in this Chrome is a clear error, not a TypeError', async () => {
  const e = env({ api: {} });
  await assert.rejects(executeCall({ namespace: 'tabGroups', method: 'query', args: [{}] }, e), /chrome\.tabGroups\.query is unavailable/);
});

test('undefined results become null so the daemon always gets a JSON value', async () => {
  const e = env({ owned: [1], api: { tabs: { reload: async () => undefined } } });
  assert.deepEqual(await executeCall({ namespace: 'tabs', method: 'reload', args: [1] }, e), { result: null });
});

test('the advertised policy is the enforced one', () => {
  const s = policySummary();
  assert.equal(s.version, POLICY_VERSION);
  assert.deepEqual(Object.keys(s.namespaces), Object.keys(ALLOWED));
  for (const [ns, { read, mutate }] of Object.entries(ALLOWED)) {
    assert.deepEqual(s.namespaces[ns], { read, mutate });
  }
  // Mutating the summary must not mutate the policy.
  s.namespaces.tabs.mutate.push('executeScript');
  assert.equal(validateCall({ namespace: 'tabs', method: 'executeScript' }).ok, false);
});
