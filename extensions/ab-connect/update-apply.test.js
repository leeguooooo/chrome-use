// #524: a downloaded update used to wait for "no attached tab", which a
// long-lived driven tab never gives, so a Chrome sat on an old build for good.
// Pure rules first, then the real background.js handlers in a vm with a fake
// chrome and a fake clock: an idle attached tab is released and the update
// applied; a command in flight holds it until it is done; the tabs the
// extension created stay owned across its own update.
import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'

import {
  UPDATE_HANDOFF_KEY,
  UPDATE_HANDOFF_MAX_AGE_MS,
  UPDATE_IDLE_GRACE_MS,
  UPDATE_CHECK_INTERVAL_MS,
  canApplyUpdateNow,
  keepsOwnershipAcrossUpdate,
  shouldCheckForUpdate,
  updateApplyPlan,
} from './update-check.js'

const GRACE = UPDATE_IDLE_GRACE_MS

test('the grace is short but not instant', () => {
  assert.ok(GRACE >= 30_000 && GRACE <= 120_000, `${GRACE}`)
})

test('an idle attached tab no longer holds an update back for good', () => {
  const now = 1_000_000
  const tabs = new Map([[7, { attached: true, inflight: 0, lastActivity: now - GRACE - 1 }]])
  assert.equal(canApplyUpdateNow(tabs.entries()), false, 'the old rule never let it through')
  const plan = updateApplyPlan(tabs.entries(), { now, lastActivityAt: now - GRACE - 5 })
  assert.equal(plan.apply, true)
  assert.equal(plan.reason, 'idle')
  assert.equal(plan.attachedTabs, 1)
})

test('recent activity makes it wait, and says how long', () => {
  const now = 1_000_000
  const tabs = new Map([[7, { attached: true, inflight: 0, lastActivity: now - 50_000 }]])
  const plan = updateApplyPlan(tabs.entries(), { now, lastActivityAt: now - 10_000 })
  assert.equal(plan.apply, false)
  assert.equal(plan.reason, 'recent_activity')
  assert.equal(plan.idleForMs, 10_000)
  assert.equal(plan.appliesInMs, GRACE - 10_000)
  // A tab's own activity counts as well as the host's.
  const tab = new Map([[7, { attached: true, inflight: 0, lastActivity: now - 1_000 }]])
  assert.equal(updateApplyPlan(tab.entries(), { now, lastActivityAt: 0 }).idleForMs, 1_000)
})

test('nothing is ever applied under a command, however long it has been quiet', () => {
  const now = 1_000_000
  const old = now - 10 * GRACE
  for (const [entries, opts] of [
    [[[7, { attached: true, inflight: 1, lastActivity: old }]], {}],
    [[[7, { attached: false, inflight: 1, lastActivity: old }]], {}],
    [[[7, { attached: false, inflight: 0, reattaching: Promise.resolve() }]], {}],
    [[], { commandsInFlight: 1 }],
  ]) {
    const plan = updateApplyPlan(new Map(entries).entries(), { now, lastActivityAt: old, ...opts })
    assert.equal(plan.apply, false, JSON.stringify(entries))
    assert.equal(plan.reason, 'command_in_flight')
    assert.equal(plan.appliesInMs, null)
    assert.ok(plan.commandsInFlight >= 1)
  }
  // A tab command inside a host command is one command, not two.
  const one = updateApplyPlan(new Map([[7, { attached: true, inflight: 1 }]]).entries(),
    { now, lastActivityAt: old, commandsInFlight: 1 })
  assert.equal(one.commandsInFlight, 1)
})

test('with nothing attached it applies at once, as before', () => {
  const now = 1_000_000
  for (const entries of [[], [[7, { attached: false, inflight: 0 }]]]) {
    const plan = updateApplyPlan(new Map(entries).entries(), { now, lastActivityAt: now })
    assert.equal(plan.apply, true)
    assert.equal(plan.reason, 'nothing_attached')
  }
})

test('only a fresh handoff from our own update keeps the created tabs owned', () => {
  const now = 5_000_000
  assert.equal(keepsOwnershipAcrossUpdate('update', { at: now - 2_000 }, now), true)
  assert.equal(keepsOwnershipAcrossUpdate('update', { at: now - UPDATE_HANDOFF_MAX_AGE_MS - 1 }, now), false)
  assert.equal(keepsOwnershipAcrossUpdate('update', null, now), false, 'Chrome applied it itself')
  assert.equal(keepsOwnershipAcrossUpdate('install', { at: now }, now), false)
  assert.equal(keepsOwnershipAcrossUpdate('update', { at: 'x' }, now), false)
  assert.equal(keepsOwnershipAcrossUpdate('update', { at: now + 10 * 60_000 }, now), false)
})

// ---- the real handlers ------------------------------------------------------

const SOURCE = readFileSync(new URL('./background.js', import.meta.url), 'utf8')

function slice(start, end) {
  const from = SOURCE.indexOf(start)
  assert.ok(from >= 0, `background.js has ${start}`)
  const to = SOURCE.indexOf(end, from)
  assert.ok(to > from, `background.js has ${end} after ${start}`)
  return SOURCE.slice(from, to)
}

function deferred() {
  let resolve, reject
  const promise = new Promise((res, rej) => { resolve = res; reject = rej })
  return { promise, resolve, reject }
}

const flush = async () => { for (let i = 0; i < 100; i++) await Promise.resolve() }

// A worker at time T0 with the given tab entries, a fake clock and timers, and
// a fake chrome that records every call that could disturb the user.
function worker({ tabs = [], storage = {} } = {}) {
  const T0 = 10_000_000
  const clock = { now: T0 }
  const timers = []
  const calls = { reload: 0, detach: [], disturbing: [], storageSet: [], posted: [] }
  const store = { ...storage }
  const recordDisturbing = (name) => (...args) => {
    calls.disturbing.push([name, ...args])
    return Promise.resolve({})
  }
  const context = {
    console,
    Promise,
    Map,
    Set,
    Math,
    Number,
    JSON,
    Date: { now: () => clock.now },
    setTimeout: (fn, ms) => {
      const t = { at: clock.now + (ms || 0), fn }
      timers.push(t)
      return t
    },
    clearTimeout: (t) => {
      const i = timers.indexOf(t)
      if (i >= 0) timers.splice(i, 1)
    },
    tabs: new Map(tabs),
    childSessionToTab: new Map(),
    ownedTabs: new Set(),
    withRelayTimeout: (p) => p,
    postToHost: (msg) => calls.posted.push(msg),
    scheduleKeepalivePing() {},
    reannounceAttachedTabs: async () => {},
    reattachOwnedTabs: async () => {},
    syncRelayTargets() {},
    reconcileAttachedTabEntries: async () => ({ live: [], removed: [] }),
    isMissingTabError: () => false,
    removedTabs: new Set(),
    detachTab() {},
    unmarkOwned() {},
    loadOwnedTabs: async () => {},
    keepsOwnershipAcrossUpdate,
    updateApplyPlan,
    shouldCheckForUpdate,
    UPDATE_HANDOFF_KEY,
    // Set per test: what the worker does with a forwarded command.
    handleForwardCdpCommand: async () => ({}),
    chrome: {
      runtime: {
        reload: () => { calls.reload++ },
        // What Chrome's update check answers; set per test.
        requestUpdateCheck: async () => context.updateCheckAnswer ?? { status: 'no_update' },
        onStartup: { addListener() {} },
        getManifest: () => ({ version: '0.5.35' }),
      },
      debugger: {
        detach: async ({ tabId }) => { calls.detach.push(tabId) },
        attach: recordDisturbing('debugger.attach'),
      },
      storage: {
        local: {
          set: async (obj) => {
            calls.storageSet.push(obj)
            if (context.storageSetGate) await context.storageSetGate
            if (context.storageFails) throw new Error('storage unavailable')
            Object.assign(store, obj)
          },
          get: async (key) => ({ [key]: store[key] }),
          remove: async (key) => { delete store[key] },
        },
      },
      // Anything here would activate, focus or move a tab or window.
      tabs: {
        update: recordDisturbing('tabs.update'),
        highlight: recordDisturbing('tabs.highlight'),
        move: recordDisturbing('tabs.move'),
        remove: recordDisturbing('tabs.remove'),
      },
      windows: { update: recordDisturbing('windows.update') },
    },
  }
  vm.createContext(context)
  for (const [start, end] of [
    ['async function onHostMessage(', '// ---- CDP command dispatch'],
    ['async function settleOwnershipAfterInstall(', '// ---- self-update'],
    ['let lastUpdateCheckAt = 0;', 'chrome.runtime.onUpdateAvailable.addListener('],
    ['function maybeCheckForUpdate(', '// Wake-from-sleep'],
  ]) vm.runInContext(slice(start, end), context)
  const read = (expr) => vm.runInContext(expr, context)
  return {
    context,
    calls,
    store,
    read,
    T0,
    async advance(ms) {
      clock.now += ms
      for (;;) {
        timers.sort((a, b) => a.at - b.at)
        const due = timers[0]
        if (!due || due.at > clock.now) break
        timers.shift()
        due.fn()
        await flush()
      }
      await flush()
    },
    async updateAvailable(version = '0.5.36') {
      context.__abConnectSimulateUpdateAvailable(version)
      await flush()
    },
    command(method = 'Runtime.evaluate') {
      return context.onHostMessage({
        id: 1,
        method: 'forwardCDPCommand',
        params: { method, sessionId: 'cb-tab-7' },
      })
    },
    state: () => read('updateStateSnapshot()'),
  }
}

const idleTab = (lastActivity) => [7, {
  sessionId: 'cb-tab-7', targetId: 'T7', attached: true, inflight: 0, lastActivity, replay: new Map(),
}]

test('pending update + idle attached tab: the tab is released and the update applied', async () => {
  const w = worker({ tabs: [idleTab(10_000_000 - 3_600_000)] })
  // The worker just started: that counts as activity, so it waits the grace.
  await w.updateAvailable('0.5.36')
  assert.equal(w.calls.reload, 0)
  let s = w.state()
  assert.equal(s.pending, true)
  assert.equal(s.version, '0.5.36')
  assert.equal(s.reason, 'recent_activity')
  assert.equal(s.attachedTabs, 1)
  assert.ok(s.appliesInMs > 0 && s.appliesInMs <= GRACE, `${s.appliesInMs}`)

  await w.advance(GRACE - 5_000)
  assert.equal(w.calls.reload, 0, 'not before the quiet period ends')
  await w.advance(6_000)
  assert.deepEqual(w.calls.detach, [7], 'the idle tab is released')
  assert.equal(w.calls.reload, 1, 'and the update applied')
  assert.equal(w.context.tabs.get(7).attached, false)
  assert.equal(w.context.tabs.has(7), true, 'the relay record stays for the reconnect')
  assert.equal(w.store[UPDATE_HANDOFF_KEY].to, '0.5.36', 'the next worker keeps the tab owned')
  assert.equal(w.read('updatePending'), false)
  assert.deepEqual(w.calls.disturbing, [], 'nothing activated, focused, moved or closed')
  assert.equal(w.calls.posted.some((m) => m.params?.method === 'Target.detachedFromTarget'), false,
    'the host is not told the tab is gone')
})

test('a command in flight holds the update until it is done, then the quiet period runs', async () => {
  const w = worker({ tabs: [idleTab(10_000_000 - 3_600_000)] })
  const running = deferred()
  w.context.handleForwardCdpCommand = () => running.promise
  await w.advance(GRACE + 1_000)
  const command = w.command()
  await flush()
  await w.updateAvailable()
  assert.equal(w.calls.reload, 0)
  assert.equal(w.state().reason, 'command_in_flight')
  assert.equal(w.state().commandsInFlight, 1)
  // However long the command runs, nothing is released or reloaded under it.
  await w.advance(10 * GRACE)
  assert.equal(w.calls.reload, 0)
  assert.deepEqual(w.calls.detach, [])
  assert.equal(w.context.tabs.get(7).attached, true)

  running.resolve({ ok: true })
  await command
  assert.deepEqual(w.calls.posted.map((m) => m.id), [1], 'the command answered')
  assert.equal(w.state().reason, 'recent_activity')
  await w.advance(GRACE - 1_000)
  assert.equal(w.calls.reload, 0, 'the quiet period starts when the command ends')
  await w.advance(2_000)
  assert.equal(w.calls.reload, 1)
  assert.deepEqual(w.calls.detach, [7])
  assert.deepEqual(w.calls.disturbing, [])
})

test('a command that arrives while the update is being applied stops the reload', async () => {
  const w = worker({ tabs: [idleTab(0)] })
  const gate = deferred()
  w.context.storageSetGate = gate.promise
  const running = deferred()
  w.context.handleForwardCdpCommand = () => running.promise
  await w.advance(GRACE + 1_000)
  await w.updateAvailable()
  assert.equal(w.calls.storageSet.length, 1, 'writing the handoff')
  const command = w.command()
  gate.resolve()
  await flush()
  assert.equal(w.calls.reload, 0, 'never under the new command')
  assert.deepEqual(w.calls.detach, [], 'and its tab is not released')
  running.resolve({})
  await command
  await w.advance(GRACE + 1_000)
  assert.equal(w.calls.reload, 1, 'the next quiet period applies it')
})

test('no handoff note, no reload: a failed write is retried later, not in a loop', async () => {
  const w = worker({ tabs: [idleTab(0)] })
  w.context.storageFails = true
  await w.advance(GRACE + 1_000)
  await w.updateAvailable()
  assert.equal(w.calls.storageSet.length, 1)
  assert.equal(w.calls.reload, 0, 'the next worker would forget the session tabs')
  assert.deepEqual(w.calls.detach, [], 'and nothing is released')
  assert.equal(w.context.tabs.get(7).attached, true)
  // The keepalive alarm calls in again; within the back-off nothing is tried.
  w.context.storageFails = false
  await w.advance(10_000)
  await w.read('applyUpdateWhenIdle()')
  await w.advance(0)
  assert.equal(w.calls.storageSet.length, 1)
  await w.advance(25_000)
  await w.read('applyUpdateWhenIdle()')
  await w.advance(0)
  assert.equal(w.calls.storageSet.length, 2)
  assert.equal(w.calls.reload, 1)
  assert.deepEqual(w.calls.detach, [7])
})

test('no onUpdateAvailable at all: the periodic check finds the update and it applies when quiet', async () => {
  const w = worker({ tabs: [idleTab(0)] })
  // First check (worker start): nothing yet. It is remembered for status.
  w.read('maybeCheckForUpdate()')
  await w.advance(0)
  let s = w.state()
  assert.equal(s.pending, false)
  assert.equal(s.lastCheck.status, 'no_update')
  // Within the interval Chrome is not asked again.
  w.context.updateCheckAnswer = { status: 'update_available', version: '0.5.36' }
  await w.advance(10 * 60_000)
  w.read('maybeCheckForUpdate()')
  await w.advance(0)
  assert.equal(w.state().lastCheck.status, 'no_update')
  // The next check finds the downloaded update; the idle tab no longer holds it.
  await w.advance(UPDATE_CHECK_INTERVAL_MS)
  w.read('maybeCheckForUpdate()')
  await w.advance(0)
  s = w.state()
  assert.equal(s.lastCheck.status, 'update_available')
  assert.equal(s.lastCheck.version, '0.5.36')
  assert.equal(w.calls.reload, 1, 'applied: the relay had been quiet for over a minute')
  assert.deepEqual(w.calls.detach, [7])
  assert.deepEqual(w.calls.disturbing, [])
})

test('a failing update check is reported, not hidden', async () => {
  const w = worker()
  w.context.chrome.runtime.requestUpdateCheck = async () => { throw new Error('no update url') }
  w.read('maybeCheckForUpdate()')
  await w.advance(0)
  assert.equal(w.state().lastCheck.status, 'error')
  assert.match(w.state().lastCheck.error, /no update url/)
})

test('a state read (status, doctor) does not count as a session working', async () => {
  const w = worker({ tabs: [idleTab(0)] })
  w.context.handleForwardCdpCommand = async () => ({})
  await w.advance(GRACE - 10_000)
  await w.updateAvailable()
  await w.advance(5_000)
  await w.command('ABExt.state')
  await w.advance(6_000)
  assert.equal(w.calls.reload, 1)
})

test('with no tab attached the update applies at once', async () => {
  const w = worker({ tabs: [[7, { attached: false, inflight: 0, lastActivity: 10_000_000 }]] })
  await w.updateAvailable()
  assert.equal(w.calls.reload, 1)
  assert.deepEqual(w.calls.detach, [])
})

test('after our own update the created tabs stay owned; otherwise they are purged', async () => {
  for (const [label, handoff, kept] of [
    ['own update', { at: 10_000_000 - 3_000 }, true],
    ['Chrome applied it', undefined, false],
    ['stale handoff', { at: 10_000_000 - UPDATE_HANDOFF_MAX_AGE_MS - 1 }, false],
  ]) {
    const w = worker({ storage: { ab_owned_tabs: [7, 8], [UPDATE_HANDOFF_KEY]: handoff } })
    w.context.ownedTabs.add(7)
    w.context.ownedTabs.add(8)
    await w.context.settleOwnershipAfterInstall('update')
    await flush()
    assert.equal(w.context.ownedTabs.size, kept ? 2 : 0, label)
    assert.equal(w.store.ab_owned_tabs !== undefined, kept, label)
    assert.equal(w.store[UPDATE_HANDOFF_KEY], undefined, `${label}: the note is used once`)
  }
  const w = worker({ storage: { ab_owned_tabs: [7], [UPDATE_HANDOFF_KEY]: { at: 10_000_000 } } })
  w.context.ownedTabs.add(7)
  await w.context.settleOwnershipAfterInstall('install')
  assert.equal(w.context.ownedTabs.size, 0, 'a fresh install starts clean')
})
