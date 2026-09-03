import assert from 'node:assert/strict'
import test from 'node:test'

import {
  activeReloadLoop,
  newReloadState,
  recordNavigationCommit,
  resetReloadLoop,
  transferReloadState,
} from './reload-loop.js'

test('four rapid commits to the same URL expose a reload loop (#211)', () => {
  const state = newReloadState()
  assert.equal(recordNavigationCommit(state, 'https://example.com/apps', 1000), null)
  assert.equal(recordNavigationCommit(state, 'https://example.com/apps', 2000), null)
  assert.equal(recordNavigationCommit(state, 'https://example.com/apps', 3000), null)
  assert.deepEqual(recordNavigationCommit(state, 'https://example.com/apps', 4000), {
    url: 'https://example.com/apps',
    count: 4,
    windowMs: 8000,
  })
})

test('different URLs and explicit recovery reset the detector', () => {
  const state = newReloadState()
  for (const at of [1000, 2000, 3000]) recordNavigationCommit(state, 'https://example.com/a', at)
  assert.equal(recordNavigationCommit(state, 'https://example.com/b', 4000), null)
  resetReloadLoop(state)
  assert.equal(activeReloadLoop(state, 4500), null)
})

test('a detected loop expires after the page stays put', () => {
  const state = newReloadState()
  for (const at of [1000, 2000, 3000, 4000]) {
    recordNavigationCommit(state, 'https://example.com/apps', at)
  }
  assert.ok(activeReloadLoop(state, 9000))
  assert.equal(activeReloadLoop(state, 13000), null)
})

test('stable-target recovery retains commits before the replacement attaches', () => {
  const states = new Map([[10, newReloadState()]])
  const original = states.get(10)
  for (const at of [1000, 2000, 3000]) recordNavigationCommit(original, 'https://example.com/apps', at)
  const replacement = transferReloadState(states, 10, 20)
  assert.equal(replacement, original)
  assert.equal(states.has(10), false)
  assert.equal(states.get(20), original)
  assert.equal(recordNavigationCommit(states.get(20), 'https://example.com/apps', 4000).count, 4)
  assert.equal(transferReloadState(states, 10, 20), original)
})
