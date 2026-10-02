import test from 'node:test'
import assert from 'node:assert/strict'
import { followReplacement, forgetReplacement, recordReplacement, reviveDiscardedTab } from './tab-replacement.js'

test('followReplacement walks old -> new and stops on unknown ids', () => {
  const m = new Map([[1, 2], [2, 3]])
  assert.equal(followReplacement(m, 1), 3)
  assert.equal(followReplacement(m, 3), 3)
  assert.equal(followReplacement(m, 9), 9)
})

test('followReplacement survives a cycle', () => {
  const m = new Map([[1, 2], [2, 1]])
  assert.ok([1, 2].includes(followReplacement(m, 1)))
})

test('reviveDiscardedTab reloads in place and waits for the load', async () => {
  const calls = []
  let polls = 0
  const api = {
    reload: async (id) => calls.push(['reload', id]),
    get: async (id) => (++polls < 3 ? { id, discarded: true, status: 'loading' } : { id, discarded: false, status: 'complete' }),
  }
  assert.equal(await reviveDiscardedTab(api, 7, { stepMs: 1 }), true)
  assert.deepEqual(calls, [['reload', 7]])
})

test('reviveDiscardedTab gives up on a closed tab', async () => {
  const api = { reload: async () => {}, get: async () => { throw new Error('No tab with id') } }
  assert.equal(await reviveDiscardedTab(api, 7, { stepMs: 1 }), false)
})

test('recordReplacement flattens chains and stays bounded', () => {
  const m = new Map()
  recordReplacement(m, 1, 2)
  recordReplacement(m, 2, 3)
  assert.equal(m.get(1), 3)
  assert.equal(followReplacement(m, 1), 3)
  for (let i = 100; i < 200; i++) recordReplacement(m, i, i + 1000, 8)
  assert.equal(m.size, 8)
  assert.equal(m.has(1), false)
  assert.equal(followReplacement(m, 199), 1199)
})

test('forgetReplacement drops a followed chain only', () => {
  const m = new Map([[1, 2], [2, 3], [7, 8]])
  forgetReplacement(m, 1)
  assert.deepEqual([...m], [[7, 8]])
})
