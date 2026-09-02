import assert from 'node:assert/strict'
import test from 'node:test'

import { idleDetachMsFrom, rememberReplayable, selectIdleTabs } from './idle-detach.js'

test('idle setting parses seconds, 0 disables, garbage falls back to the default (#201)', () => {
  assert.equal(idleDetachMsFrom(45), 45000)
  assert.equal(idleDetachMsFrom('10'), 10000)
  assert.equal(idleDetachMsFrom(0), 0)
  assert.equal(idleDetachMsFrom(-3), 30000)
  assert.equal(idleDetachMsFrom(undefined), 30000)
})

test('only quiet, attached, non-busy tabs are released', () => {
  const now = 100000
  const entries = [
    [1, { attached: true, inflight: 0, lastActivity: now - 31000 }], // idle → release
    [2, { attached: true, inflight: 0, lastActivity: now - 5000 }], // recent
    [3, { attached: true, inflight: 1, lastActivity: now - 60000 }], // command running
    [4, { attached: false, inflight: 0, lastActivity: now - 60000 }], // already released
    [5, { attached: true, inflight: 0, lastActivity: now - 60000, reattaching: Promise.resolve() }],
  ]
  assert.deepEqual(selectIdleTabs(entries, now, 30000), [1])
  assert.deepEqual(selectIdleTabs(entries, now, 0), [], '0 = never detach')
})

test('replay map keeps enables and emulation state, drops disabled domains and one-shots', () => {
  const replay = new Map()
  rememberReplayable(replay, 'Page.enable')
  rememberReplayable(replay, 'Network.enable')
  rememberReplayable(replay, 'Emulation.setFocusEmulationEnabled', { enabled: true })
  rememberReplayable(replay, 'Emulation.setFocusEmulationEnabled', { enabled: false })
  rememberReplayable(replay, 'Page.navigate', { url: 'https://example.com' })
  rememberReplayable(replay, 'Runtime.evaluate', { expression: '1' })
  rememberReplayable(replay, 'Page.addScriptToEvaluateOnNewDocument', { source: 'a' })
  rememberReplayable(replay, 'Page.addScriptToEvaluateOnNewDocument', { source: 'b' })
  rememberReplayable(replay, 'Network.disable')
  const replayed = [...replay.values()].map((c) => [c.method, c.params])
  assert.deepEqual(replayed, [
    ['Page.enable', undefined],
    ['Emulation.setFocusEmulationEnabled', { enabled: false }],
    ['Page.addScriptToEvaluateOnNewDocument', { source: 'a' }],
    ['Page.addScriptToEvaluateOnNewDocument', { source: 'b' }],
  ])
})
