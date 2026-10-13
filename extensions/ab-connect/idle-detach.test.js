import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'

import { idleDetachMsFrom, rememberReplayable, selectIdleTabs } from './idle-detach.js'

const background = readFileSync(new URL('./background.js', import.meta.url), 'utf8')

function idleWorker(getTab) {
  const released = []
  const context = {
    tabs: new Map([[1, { attached: true, inflight: 0, lastActivity: 0 }]]),
    Date: { now: () => 100000 }, idleDetachMs: 30000, selectIdleTabs,
    chrome: { tabs: { get: getTab } },
    softDetachTab: id => released.push(id),
  }
  vm.createContext(context)
  const start = background.indexOf('async function sweepIdleTabs(')
  const end = background.indexOf('function detachTab(', start)
  assert.ok(start >= 0 && end > start)
  vm.runInContext(background.slice(start, end), context)
  return { context, released }
}

test('real idle sweep preserves active tabs and unknown status, but releases quiet background tabs (#548)', async () => {
  for (const status of [{ active: true }, null, { active: false }]) {
    const w = idleWorker(async () => status)
    await w.context.sweepIdleTabs()
    assert.deepEqual(w.released, status?.active === false ? [1] : [])
  }
  const failed = idleWorker(async () => { throw new Error('tab status unavailable') })
  await failed.context.sweepIdleTabs()
  assert.deepEqual(failed.released, [])
})

test('idle sweep rechecks activity and in-flight commands after reading Chrome tab status', async () => {
  for (const update of [entry => { entry.inflight = 1 }, entry => { entry.lastActivity = 100000 }]) {
    const w = idleWorker(async () => { update(w.context.tabs.get(1)); return { active: false } })
    await w.context.sweepIdleTabs()
    assert.deepEqual(w.released, [])
  }
})

test('idle setting parses seconds, 0 disables, garbage falls back to the default (#201)', () => {
  assert.equal(idleDetachMsFrom(45), 45000)
  assert.equal(idleDetachMsFrom('10'), 10000)
  assert.equal(idleDetachMsFrom(0), 0)
  assert.equal(idleDetachMsFrom(-3), 0)
  assert.equal(idleDetachMsFrom(undefined), 0)
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
