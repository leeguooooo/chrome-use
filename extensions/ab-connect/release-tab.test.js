import assert from 'node:assert/strict'
import test from 'node:test'

import { releaseTab } from './release-tab.js'

function deps({ owned = [], attached = { T1: 11 }, detachError = null } = {}) {
  const calls = { detach: [], forget: [] }
  return {
    calls,
    tabForTarget: (t) => (t in attached ? attached[t] : null),
    isOwned: (id) => owned.includes(id),
    detach: async (id) => {
      calls.detach.push(id)
      if (detachError) throw new Error(detachError)
    },
    forget: (id) => calls.forget.push(id),
  }
}

test('a user tab the relay holds is detached and forgotten', async () => {
  const d = deps()
  assert.deepEqual(await releaseTab({ targetId: 'T1' }, d), { released: true, tabId: 11 })
  assert.deepEqual(d.calls.detach, [11])
  assert.deepEqual(d.calls.forget, [11])
})

test('an agent-created tab is never released here', async () => {
  const d = deps({ owned: [11] })
  assert.deepEqual(await releaseTab({ targetId: 'T1' }, d), {
    released: false,
    tabId: 11,
    reason: 'agent-owned',
  })
  assert.deepEqual(d.calls.detach, [])
  assert.deepEqual(d.calls.forget, [])
})

test('a target the relay does not hold is reported, not guessed', async () => {
  const d = deps()
  assert.deepEqual(await releaseTab({ targetId: 'T9' }, d), {
    released: false,
    reason: 'not-attached',
  })
  assert.deepEqual(await releaseTab({}, d), { released: false, reason: 'bad-request' })
  assert.deepEqual(d.calls.detach, [])
})

test('an already-detached tab counts as released; another error does not', async () => {
  const gone = deps({ detachError: 'Debugger is not attached to the tab with id: 11.' })
  assert.deepEqual(await releaseTab({ targetId: 'T1' }, gone), { released: true, tabId: 11 })
  const busy = deps({ detachError: 'Tabs cannot be edited right now' })
  const r = await releaseTab({ targetId: 'T1' }, busy)
  assert.equal(r.released, false)
  assert.match(r.reason, /cannot be edited/)
  assert.deepEqual(busy.calls.forget, [])
})
