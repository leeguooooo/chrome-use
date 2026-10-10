import assert from 'node:assert/strict';
import test from 'node:test';
import { withRelayTimeout } from './relay-timeout.js';

let instance = 0;
const fresh = () => import(`./relay-health.js?test=${instance++}`);

test('success and non-timeout rejection are answers and preserve identity', async () => {
  const { observeDebuggerCommand: observe, getRelayHealth: health } = await fresh();
  const result = {};
  const error = new Error('debugger rejected command');
  assert.equal(await observe('Page.enable', Promise.resolve(result), () => 1000), result);
  await assert.rejects(observe('Page.enable', Promise.reject(error), () => 1000), e => e === error);
  assert.deepEqual(health(1000), { windowMs: 600000, workerAgeMs: 0, answered: 2, timedOut: 0, lastTimeout: null });
});

test('a real relay timeout is counted and its rejection passes through unchanged', async () => {
  const { observeDebuggerCommand: observe, getRelayHealth: health } = await fresh();
  let original;
  const promise = withRelayTimeout(new Promise(() => {}), 'Runtime.evaluate', 1)
    .catch(error => { original = error; throw error; });
  await assert.rejects(observe('Runtime.evaluate', promise, () => 2000), e => e === original);
  assert.deepEqual(health(3000).lastTimeout, { method: 'Runtime.evaluate', ageMs: 1000 });
  assert.equal(health(3000).timedOut, 1);
  assert.equal(health(3000).answered, 0);
  assert.equal(health(602000).timedOut, 0);
  assert.equal(health(602000).lastTimeout, null);
});

test('fixed ring retains burst counts and overwrites expired buckets across many windows', async () => {
  const { observeDebuggerCommand: observe, getRelayHealth: health } = await fresh();
  for (let i = 0; i < 20000; i++) await observe('Page.enable', Promise.resolve(), () => 1000);
  assert.equal(health(1000).answered, 20000);
  for (let second = 2; second <= 2000; second++) {
    await observe('Page.enable', Promise.resolve(), () => second * 1000);
  }
  assert.equal(health(2000000).answered, 600);
  assert.equal(health(2600000).answered, 0);
});

test('worker age measures this module lifetime', async () => {
  const before = Date.now();
  const { getRelayHealth } = await fresh();
  const now = Date.now() + 2000;
  const age = getRelayHealth(now).workerAgeMs;
  assert.ok(age >= 2000 && age <= now - before);
});
