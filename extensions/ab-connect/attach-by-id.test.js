import { test } from 'node:test';
import assert from 'node:assert/strict';
import { attachTabById } from './attach-by-id.js';

const eligible = (t) => typeof t.url === 'string' && t.url !== '' && !/^about:/.test(t.url);

function deps(tabs, attached) {
  return {
    getTab: async (id) => {
      const t = tabs.get(id);
      if (!t) throw new Error('No tab with id');
      return t;
    },
    eligible,
    attachTab: async (id) => {
      attached.push(id);
      return { targetId: `T${id}` };
    },
  };
}

test('attaches exactly the named tab, even when another tab shows the same URL', async () => {
  const url = 'http://h/popup-busy.html';
  const tabs = new Map([
    [1, { id: 1, url }], // the user's tab, same URL, listed first
    [2, { id: 2, url }],
  ]);
  const attached = [];
  const out = await attachTabById({ chromeTabId: 2 }, deps(tabs, attached));
  assert.deepEqual(attached, [2]);
  assert.equal(out.targetId, 'T2');
  assert.equal(out.chromeTabId, 2);
  assert.equal(out.attached, true);
});

test('a blank or privileged tab is reported, not attached', async () => {
  const tabs = new Map([[3, { id: 3, url: 'about:blank', pendingUrl: 'http://h/p' }]]);
  const attached = [];
  const out = await attachTabById({ chromeTabId: 3 }, deps(tabs, attached));
  assert.deepEqual(attached, []);
  assert.equal(out.attached, false);
});

test('a missing tab or a non-id is an error and attaches nothing', async () => {
  const attached = [];
  await assert.rejects(attachTabById({ chromeTabId: 9 }, deps(new Map(), attached)), /does not exist/);
  await assert.rejects(attachTabById({ chromeTabId: 'http://h/p' }, deps(new Map(), attached)), /tab id/);
  await assert.rejects(attachTabById({}, deps(new Map(), attached)), /tab id/);
  assert.deepEqual(attached, []);
});
