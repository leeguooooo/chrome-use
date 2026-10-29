import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import { rememberReplayable } from './idle-detach.js'
import { resolveSessionTab } from './tab-liveness.js'

const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8')
const start = source.indexOf('  // Everything else → chrome.debugger on the resolved tab.')
const end = source.indexOf('async function dispatchToTab(', start)
assert.ok(start >= 0 && end > start)

function worker(dispatch) {
  const entry = { attached: true, inflight: 0, replay: new Map() }
  rememberReplayable(entry.replay, 'Emulation.setDeviceMetricsOverride', { width: 390, height: 844, mobile: true })
  rememberReplayable(entry.replay, 'Emulation.setUserAgentOverride', { userAgent: 'MobileFixture/1.0' })
  rememberReplayable(entry.replay, 'Emulation.setTouchEmulationEnabled', { enabled: true })
  const context = {
    tabs: new Map([[7, entry]]), sessionToTab: new Map([['cb-tab-7', 7]]), childSessionToTab: new Map([['frame-7', 7]]),
    resolveSessionTab, tabIdFromSession: sid => sid === 'cb-tab-7' ? 7 : null,
    guardTab() {}, activeReloadLoop: () => null,
    attachmentHealth: { recover: async () => false }, tabCommandDependencies: () => ({}),
    rememberReplayable: (e, method, params) => { if (e) rememberReplayable(e.replay, method, params) },
    dispatchToTab: dispatch,
  }
  vm.createContext(context)
  vm.runInContext(`async function forward(method, params, sessionId) { const agentSession = 'fixture';\n${source.slice(start, end)}`, context)
  return { entry, forward: (method = 'Emulation.clearDeviceMetricsOverride', params = {}, session = 'cb-tab-7') => context.forward(method, params, session) }
}

test('the real background forgets the old device only after Chrome confirms its clear', async () => {
  let finish
  const dispatched = new Promise(resolve => { finish = resolve })
  const w = worker(() => dispatched)
  const clearing = w.forward()
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(w.entry.inflight, 1)
  assert.equal(w.entry.replay.has('Emulation.setDeviceMetricsOverride'), true)
  finish({})
  await clearing
  assert.equal(w.entry.inflight, 0)
  assert.equal(w.entry.replay.has('Emulation.setDeviceMetricsOverride'), false)
  assert.equal(w.entry.replay.has('Emulation.clearDeviceMetricsOverride'), false)
})

test('refused and unknown-outcome clears leave the last known device replay intact', async () => {
  for (const message of ['debugger_access_denied', 'action_outcome_unknown: clear may have run']) {
    const w = worker(async () => { throw new Error(message) })
    await assert.rejects(w.forward(), error => error.message === message)
    assert.equal(w.entry.inflight, 0)
    assert.deepEqual(w.entry.replay.get('Emulation.setDeviceMetricsOverride').params, { width: 390, height: 844, mobile: true })
  }
})

test('clearing a child frame never discards the parent tab device override', async () => {
  const w = worker(async () => ({}))
  await w.forward('Emulation.clearDeviceMetricsOverride', {}, 'frame-7')
  assert.equal(w.entry.replay.has('Emulation.setDeviceMetricsOverride'), true)
})


for (const [method, params] of [
  ['Emulation.setUserAgentOverride', { userAgent: '' }],
  ['Emulation.setTouchEmulationEnabled', { enabled: false }],
]) {
  test(`${method} reset enters replay only after acknowledgement`, async () => {
    let finish
    const pending = new Promise(resolve => { finish = resolve })
    const w = worker(() => pending)
    const before = JSON.stringify([...w.entry.replay])
    const resetting = w.forward(method, params)
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(JSON.stringify([...w.entry.replay]), before)
    finish({})
    await resetting
    assert.deepEqual(w.entry.replay.get(method).params, params)
    assert.equal(w.entry.inflight, 0)
  })

  test(`${method} rejected or unknown reset preserves prior replay`, async () => {
    for (const message of ['debugger_access_denied', 'action_outcome_unknown: reset may have run']) {
      const w = worker(async () => { throw new Error(message) })
      const before = JSON.stringify([...w.entry.replay])
      await assert.rejects(w.forward(method, params), error => error.message === message)
      assert.equal(JSON.stringify([...w.entry.replay]), before)
      assert.equal(w.entry.inflight, 0)
    }
  })
}
