// #519: the host's tab records must follow the worker's. Pure helpers first,
// then the real background.js handlers run in a vm with a fake chrome, to show
// a tab closed mid-announce or mid-attach leaves no record anywhere.
import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'

import {
  REMOVED_TAB_LIMIT,
  announceVerdict,
  createRemovedTabs,
  relayTargetsMessage,
  tabGoneEvent,
  tabSessionId,
} from './relay-records.js'
import { isMissingTabError } from './tab-presence.js'
import { canApplyUpdateNow } from './update-check.js'
import { reconcileAttachedTabEntries } from './tab-liveness.js'

test('removed tab ids are remembered, bounded, oldest first out', () => {
  const removed = createRemovedTabs(3)
  for (const id of [1, 2, 3, 4]) removed.add(id)
  assert.equal(removed.size, 3)
  assert.equal(removed.has(1), false)
  assert.equal(removed.has(4), true)
  removed.add('x')
  assert.equal(removed.size, 3, 'a non-integer id is ignored')
  assert.ok(REMOVED_TAB_LIMIT >= 1024)
})

test('the gone event names the tab session the host keys its records on', () => {
  assert.equal(tabSessionId(42), 'cb-tab-42')
  assert.deepEqual(tabGoneEvent(42), {
    method: 'forwardCDPEvent',
    params: {
      sessionId: 'cb-tab-42',
      method: 'Target.detachedFromTarget',
      params: { sessionId: 'cb-tab-42' },
    },
  })
})

test('the sync message lists every held record, idle-detached ones too', () => {
  const tabs = new Map([
    [1, { sessionId: 'cb-tab-1', targetId: 'T1', attached: true }],
    [2, { sessionId: 'cb-tab-2', targetId: 'T2', attached: false }],
    [3, { sessionId: 'cb-tab-3' }],
    [4, null],
  ])
  assert.deepEqual(relayTargetsMessage(tabs.entries()), {
    method: 'relayTargets',
    targets: [
      { targetId: 'T1', sessionId: 'cb-tab-1' },
      { targetId: 'T2', sessionId: 'cb-tab-2' },
    ],
  })
  assert.deepEqual(relayTargetsMessage(new Map()), { method: 'relayTargets', targets: [] })
})

test('an announce whose tab closed meanwhile is never sent', () => {
  const entry = {}
  assert.equal(announceVerdict({ current: entry, entry, missing: false, removed: false }), 'announce')
  assert.equal(announceVerdict({ current: undefined, entry, missing: false, removed: true }), 'gone')
  assert.equal(announceVerdict({ current: entry, entry, missing: true, removed: false }), 'gone')
  assert.equal(announceVerdict({ current: {}, entry, missing: false, removed: false }), 'superseded')
  assert.equal(announceVerdict({ current: undefined, entry, missing: false, removed: false }), 'superseded')
})

test('only Chrome\'s own missing-tab error proves a record dead; any other read failure keeps it', async () => {
  const entries = [
    [11, { sessionId: 'cb-tab-11', targetId: 'gone' }],
    [22, { sessionId: 'cb-tab-22', targetId: 'flaky' }],
    [33, { sessionId: 'cb-tab-33', targetId: 'live' }],
  ]
  const detached = []
  const { live, removed } = await reconcileAttachedTabEntries(entries, {
    getTab: async (tabId) => {
      if (tabId === 11) throw new Error('No tab with id: 11.')
      if (tabId === 22) throw new Error('Tabs cannot be edited right now')
      return { id: tabId }
    },
    isMissing: isMissingTabError,
    detach: (tabId) => detached.push(tabId),
    unmarkOwned() {},
  })
  assert.deepEqual(live.map(([id]) => id), [22, 33])
  assert.deepEqual(removed.map((r) => r.tabId), [11])
  assert.deepEqual(detached, [11])
})

test('dead attached records stop blocking an update once pruned', async () => {
  const tabs = new Map([
    [5, { sessionId: 'cb-tab-5', targetId: 'T5', attached: true, inflight: 0 }],
    [6, { sessionId: 'cb-tab-6', targetId: 'T6', attached: true, inflight: 0 }],
  ])
  assert.equal(canApplyUpdateNow(tabs.entries()), false, 'phantoms used to count as attached')
  await reconcileAttachedTabEntries([...tabs.entries()], {
    getTab: async (tabId) => { throw new Error(`No tab with id: ${tabId}.`) },
    isMissing: isMissingTabError,
    detach: (tabId) => tabs.delete(tabId),
    unmarkOwned() {},
  })
  assert.equal(tabs.size, 0)
  assert.equal(canApplyUpdateNow(tabs.entries()), true)
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

// A worker with one attached tab (7), a fake chrome whose tabs.get the test
// controls, and every message the worker posts to the host.
function workerFixture() {
  const posted = []
  const listeners = {}
  const tabReads = []
  let pending = Promise.resolve()
  const context = {
    console,
    posted,
    tabs: new Map(),
    sessionToTab: new Map(),
    childSessionToTab: new Map(),
    sessionTargets: new Map(),
    reloadStates: new Map(),
    ownedTabs: new Set(),
    nativeDuplicateTabs: new Set(),
    recentlyRemovedOwned: new Set(),
    removedTabs: createRemovedTabs(),
    attachmentHealth: { clear() {} },
    port: {},
    RELOAD_LOOP_WINDOW_MS: 1000,
    isMissingTabError,
    announceVerdict,
    tabGoneEvent,
    relayTargetsMessage,
    postToHost: (msg) => posted.push(msg),
    forgetSessionTab(map, id) { for (const [sid, tid] of map) if (tid === id) map.delete(sid) },
    forgetAgentPopup() {},
    unmarkOwned() {},
    setBadge() {},
    setTimeout() {},
    newReloadState: () => ({}),
    tabScopeHints: async () => ({ openerTargetId: '', abGroup: '' }),
    targetInfoForTab: (targets, tabId) => {
      const t = targets.find((x) => x.tabId === tabId)
      return t ? { targetId: t.id, type: 'page', url: t.url, title: t.title, attached: true } : null
    },
    withRelayTimeout: (p) => p,
    trackedDebuggerCommand: async () => ({}),
    rememberSessionTarget(sid, tid) { context.sessionTargets.set(sid, tid) },
    whenReady(fn) { pending = Promise.resolve().then(fn); return pending },
    chrome: {
      tabs: {
        get: (tabId) => {
          const d = deferred()
          tabReads.push({ tabId, ...d })
          return d.promise
        },
        onRemoved: { addListener(fn) { listeners.remove = fn } },
      },
      debugger: {
        attach: async () => {},
        detach: async () => { context.debuggerDetached = (context.debuggerDetached || 0) + 1 },
        getTargets: async () => context.chromeTargets || [],
      },
    },
  }
  context.relayKnowsTab = (tabId) =>
    context.tabs.has(tabId) || context.sessionTargets.has(`cb-tab-${tabId}`)
  vm.createContext(context)
  for (const [start, end] of [
    ['function detachTab(', 'function eligible('],
    ['async function attachTab(', 'function rememberReplayable('],
    ['async function announceAttachedTab(', 'async function reannounceAttachedTabs('],
    ['chrome.tabs.onRemoved.addListener(', '// `tabs.onUpdated`'],
  ]) vm.runInContext(slice(start, end), context)
  return {
    context,
    posted,
    tabReads,
    async remove(tabId) { listeners.remove(tabId); await pending },
    entry(tabId, targetId) {
      const e = { sessionId: `cb-tab-${tabId}`, targetId, attached: true, inflight: 0, replay: new Map() }
      context.tabs.set(tabId, e)
      context.sessionToTab.set(e.sessionId, tabId)
      context.sessionTargets.set(e.sessionId, targetId)
      return e
    },
  }
}

const kinds = (posted) => posted.map((m) => m.params?.method ?? m.method)

test('a tab closed while its announce awaits Chrome is not re-announced after its detach (#519)', async () => {
  const w = workerFixture()
  const entry = w.entry(7, 'T7')
  // tabs.onUpdated-style re-announce: starts, then awaits chrome.tabs.get.
  const announcing = w.context.announceAttachedTab(7, entry)
  assert.equal(w.tabReads.length, 1)
  // The tab closes: onRemoved tells the host before the announce resumes.
  await w.remove(7)
  w.tabReads[0].reject(new Error('No tab with id: 7.'))
  assert.equal(await announcing, 'gone')
  assert.deepEqual(kinds(w.posted), ['Target.detachedFromTarget'])
  assert.equal(w.posted[0].params.sessionId, 'cb-tab-7')
  assert.equal(w.context.tabs.size, 0)
})

test('without the fix the same interleaving announced a url-less, title-less page after its detach', async () => {
  // The pre-#519 announce, verbatim in behaviour: read the tab, then post.
  const posted = []
  const tabs = new Map([[7, { sessionId: 'cb-tab-7', targetId: 'T7' }]])
  const read = deferred()
  const oldAnnounce = async (tabId, entry) => {
    const tab = await read.promise.catch(() => null)
    posted.push({ method: 'Target.attachedToTarget', url: tab?.url || '', title: tab?.title || '' })
  }
  const announcing = oldAnnounce(7, tabs.get(7))
  tabs.delete(7)
  posted.push({ method: 'Target.detachedFromTarget' })
  read.reject(new Error('No tab with id: 7.'))
  await announcing
  assert.deepEqual(posted, [
    { method: 'Target.detachedFromTarget' },
    { method: 'Target.attachedToTarget', url: '', title: '' },
  ])
})

test('an attach that finishes after the tab closed registers and announces nothing (#519)', async () => {
  const w = workerFixture()
  w.context.chromeTargets = [{ id: 'T9', tabId: 9, type: 'page', url: 'https://x/', title: 'x' }]
  const getTargets = deferred()
  w.context.chrome.debugger.getTargets = () => getTargets.promise
  const attaching = w.context.attachTab(9)
  // tabs.onRemoved runs while attach awaits the target registry.
  await w.remove(9)
  getTargets.resolve(w.context.chromeTargets)
  await assert.rejects(attaching, /tab 9 was closed while attaching/)
  assert.equal(w.context.tabs.size, 0, 'no attached record of a dead tab is left to block an update')
  assert.equal(kinds(w.posted).includes('Target.attachedToTarget'), false)
  assert.equal(w.context.debuggerDetached, 1, 'the debugger is released')
})

test('onRemoved tells the host about a tab the worker already forgot', async () => {
  const w = workerFixture()
  // The relay knew tab 3 (a session id was remembered) but holds no entry.
  w.context.sessionTargets.set('cb-tab-3', 'T3')
  await w.remove(3)
  assert.deepEqual(kinds(w.posted), ['Target.detachedFromTarget'])
  // A user tab the relay never knew costs no message.
  await w.remove(4)
  assert.equal(w.posted.length, 1)
})

test('a live tab is announced as before', async () => {
  const w = workerFixture()
  const entry = w.entry(8, 'T8')
  const announcing = w.context.announceAttachedTab(8, entry)
  w.tabReads[0].resolve({ id: 8, url: 'https://example.com/', title: 'Example' })
  assert.equal(await announcing, 'announce')
  assert.deepEqual(kinds(w.posted), ['Target.attachedToTarget'])
  assert.equal(w.posted[0].params.params.targetInfo.url, 'https://example.com/')
})

test('the stale-session command path reports a confirmed-gone tab to the host', async () => {
  const w = workerFixture()
  const reporting = w.context.reportTabIfGone(12)
  w.tabReads[0].reject(new Error('No tab with id: 12.'))
  await reporting
  assert.deepEqual(kinds(w.posted), ['Target.detachedFromTarget'])
  assert.equal(w.posted[0].params.sessionId, 'cb-tab-12')
  assert.equal(w.context.removedTabs.has(12), true)
  // Any other failure proves nothing.
  const flaky = w.context.reportTabIfGone(13)
  w.tabReads[1].reject(new Error('Tabs cannot be edited right now'))
  await flaky
  assert.equal(w.posted.length, 1)
})
