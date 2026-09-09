import { strict as assert } from 'node:assert';
import test from 'node:test';
import { attachedTargetsFrom } from './attached-targets.js';

test('reports every known target with its attachment state', () => {
  const rows = attachedTargetsFrom(
    new Map([
      [1, { targetId: 'A', attached: true }],
      [2, { targetId: 'B', attached: false }],
      // Created but not yet confirmed is still ours — only an explicit
      // release makes it not attached.
      [3, { targetId: 'C' }],
    ])
  );
  assert.deepEqual(rows, [
    { targetId: 'A', tabId: 1, attached: true },
    { targetId: 'B', tabId: 2, attached: false },
    { targetId: 'C', tabId: 3, attached: true },
  ]);
});

test('an entry with no targetId cannot be matched to a tab and is skipped', () => {
  assert.deepEqual(attachedTargetsFrom(new Map([[1, { attached: true }]])), []);
  assert.deepEqual(attachedTargetsFrom(new Map([[1, null]])), []);
});
