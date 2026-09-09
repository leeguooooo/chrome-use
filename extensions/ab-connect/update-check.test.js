import { strict as assert } from 'node:assert';
import test from 'node:test';
import {
  UPDATE_CHECK_INTERVAL_MS,
  shouldCheckForUpdate,
  canApplyUpdateNow,
} from './update-check.js';

test('a first check always runs; later ones wait out the interval', () => {
  assert.equal(shouldCheckForUpdate(0, 1_000), true);
  assert.equal(shouldCheckForUpdate(NaN, 1_000), true);
  assert.equal(shouldCheckForUpdate(1_000, 1_000 + UPDATE_CHECK_INTERVAL_MS - 1), false);
  assert.equal(shouldCheckForUpdate(1_000, 1_000 + UPDATE_CHECK_INTERVAL_MS), true);
});

test('an attached tab blocks the reload, an idle relay allows it', () => {
  assert.equal(canApplyUpdateNow(new Map()), true);
  assert.equal(canApplyUpdateNow(new Map([[1, { attached: false }]])), true);
  // Being attached at all is enough to defer: a task may be mid-flight between
  // commands, where inflight is momentarily 0.
  assert.equal(canApplyUpdateNow(new Map([[1, { attached: true, inflight: 0 }]])), false);
  assert.equal(canApplyUpdateNow(new Map([[1, { attached: false, inflight: 2 }]])), false);
});
