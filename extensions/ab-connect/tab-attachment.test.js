import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import { attachCreatedTab, createTabAttachmentQueue } from './tab-attachment.js'

const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8')
function section(start, end) {
  const from = source.indexOf(start)
  const to = source.indexOf(end, from)
  assert.ok(from >= 0 && to > from)
  return source.slice(from, to)
}
function deferred() {
  let resolve
  const promise = new Promise(r => { resolve = r })
  return { promise, resolve }
}

test('real attach workflow shares target discovery and announcement without replacing replay state (#549)', async () => {
  const gate = deferred()
  const announced = deferred()
  const announceGate = deferred()
  const tabs = new Map()
  let attaches = 0
  let announcements = 0
  const cursorAttachments = []
  const context = {
    cursorOverlay: { attached: async id => { cursorAttachments.push(id); return true } },
    tabGrants: { load: async () => {}, isRefused: () => false, removed: async () => null, get: () => null },    console, tabs, createTabAttachmentQueue,
    chrome: {
      debugger: {
        attach: async () => { attaches++; await gate.promise },
        getTargets: async () => [{ id: 'T1', tabId: 1 }],
      },
    },
    withRelayTimeout: p => p,
    targetInfoForTab: () => ({ targetId: 'T1' }),
    tabScopeHints: async () => ({}),
    removedTabs: new Set(), sessionToTab: new Map(),
    newTabEntry: (tabId, sessionId, targetId) => ({ tabId, sessionId, targetId, attached: true, inflight: 0, replay: new Map() }),
    rememberSessionTarget() {}, setBadge() {}, port: {},
    announceAttachedTab: async (_id, entry) => {
      announcements++
      // A request arrives after the entry was registered but before announce
      // completed. It must not replace the entry or bypass this workflow.
      entry.inflight = 1
      entry.replay.set('Network.enable', {})
      announced.resolve()
      await announceGate.promise
    },
    trackedDebuggerCommand: async () => ({}),
  }
  vm.createContext(context)
  vm.runInContext(section('const attachTab = createTabAttachmentQueue', 'function newTabEntry(') + '\nglobalThis.attachTab = attachTab;', context)
  const first = context.attachTab(1)
  const second = context.attachTab(1)
  assert.equal(first, second)
  gate.resolve()
  await announced.promise
  const third = context.attachTab(1)
  assert.equal(third, first, 'a registered but unannounced entry stays in the shared workflow')
  announceGate.resolve()
  const [a, b] = await Promise.all([first, second])
  assert.equal(a, b)
  assert.equal(tabs.get(1), a)
  assert.equal(attaches, 1)
  assert.equal(announcements, 1)
  assert.deepEqual(cursorAttachments, [1])
  assert.equal(a.inflight, 1)
  assert.ok(a.replay.has('Network.enable'))
  assert.equal(await context.attachTab(1), a)
})

test('a failed attachment releases its queue and different tabs run independently', async () => {
  const gate = deferred()
  let attempts = 0
  const attach = createTabAttachmentQueue(async id => {
    if (id === 1) { attempts++; await gate.promise; if (attempts === 1) throw new Error('not ready') }
    return id
  })
  const failed = assert.rejects(attach(1), /not ready/)
  assert.equal(await attach(2), 2)
  gate.resolve()
  await failed
  assert.equal(await attach(1), 1)
})

test('an expired recovery cannot cancel or share an ordinary attachment', async () => {
  const gate = deferred()
  const calls = []
  const attach = createTabAttachmentQueue(async (_id, token) => {
    calls.push(token)
    await gate.promise
    return 'attached'
  })
  const ordinary = attach(1)
  let active = true
  const recovery = attach(1, () => active)
  const rejected = assert.rejects(recovery, /transaction cancelled/)
  active = false
  gate.resolve()
  assert.equal(await ordinary, 'attached')
  await rejected
  assert.deepEqual(calls, [undefined])
})

test('a newly created target attaches as soon as a transient startup gap ends (#554)', async () => {
  let elapsed = 0
  let attempts = 0
  const result = await attachCreatedTab(1, {
    attach: async id => { attempts++; if (elapsed < 30) throw new Error('not ready'); return { targetId: `T${id}` } },
    wait: async ms => { elapsed += ms }, permanent: () => false,
  })
  assert.equal(result.targetId, 'T1')
  assert.equal(elapsed, 75)
  assert.equal(attempts, 3)
})

test('created-tab attachment retries are bounded and permanent failures never retry', async () => {
  for (const permanent of [false, true]) {
    let attempts = 0
    let elapsed = 0
    await assert.rejects(attachCreatedTab(1, {
      attach: async () => { attempts++; throw new Error('refused') },
      wait: async ms => { elapsed += ms }, permanent: () => permanent,
    }), /refused/)
    assert.equal(attempts, permanent ? 1 : 4)
    assert.equal(elapsed, permanent ? 0 : 175)
  }
  let waited = false
  await attachCreatedTab(1, { attach: async () => ({}), wait: async () => { waited = true }, permanent: () => false })
  assert.equal(waited, false)
})

test('real createTarget announces the requested group before replying even if visual grouping fails (#550)', async () => {
  const gate = deferred()
  const announcing = deferred()
  const creatingTabGroups = new Map()
  let initialGroup
  const context = {
    tabGrants: { load: async () => {}, isRefused: () => false, removed: async () => null, get: () => null },    attachCreatedTab, creatingTabGroups, tabs: new Map(), agentSession: 'panel-test',
    conversationWindows: { observe: async () => {} },
    chrome: { tabs: { create: async () => ({ id: 1 }), get: async () => ({ id: 1, groupId: -1 }) } },
    markOwned: async id => { assert.equal(creatingTabGroups.get(id), 'agent-a') },
    groupTabInto: async () => { throw new Error('Chrome visual grouping unavailable') },
    isPermanentAttachError: () => true,
    attachTab: async id => {
      initialGroup = (await context.tabScopeHints(id)).abGroup
      announcing.resolve()
      await gate.promise
      return { targetId: 'T1' }
    },
  }
  vm.createContext(context)
  vm.runInContext(section('async function tabScopeHints(', '// Resolve a stable identifier'), context)
  vm.runInContext('async function createTarget(params) { const method = "Target.createTarget";\n' +
    section("  if (method === 'Target.createTarget') {", "  if (method === 'Target.closeTarget') {") + '\n}', context)
  const pending = context.createTarget({ url: 'https://example.com', agentGroup: 'agent-a' })
  await announcing.promise
  assert.equal(initialGroup, 'agent-a', 'the first announcement must be scoped while the command is still pending')
  gate.resolve()
  assert.equal((await pending).targetId, 'T1')
  assert.equal(creatingTabGroups.size, 0)
  context.attachTab = async () => { throw new Error('attach refused') }
  await assert.rejects(context.createTarget({ agentGroup: 'agent-a' }), /attach refused/)
  assert.equal(creatingTabGroups.size, 0, 'failed creation must not leak scope hints')
})

function detachWorker(sequence) {
  let handler, operation, timers = 0, attaches = 0
  const context = {
    tabGrants: { load: async () => {}, isRefused: () => false, removed: async () => null, get: () => null },    console,
    chrome: {
      debugger: { onDetach: { addListener: fn => { handler = fn } } },
      tabs: { get: async () => sequence.length > 1 ? sequence.shift() : sequence[0] },
    },
    whenReady: fn => { operation = fn(); return operation },
    releasedTabs: new Set(), attachmentHealth: { isRecovering: () => false },
    tabs: new Map(), nativeDuplicateTabs: new Set(), port: {},
    detachTab() {}, eligible: tab => tab.url?.startsWith('https:'),
    attachTab: async () => { attaches++ },
    isPermanentAttachError: () => false,
    setTimeout: fn => { timers++; fn() },
  }
  vm.createContext(context)
  vm.runInContext(section('chrome.debugger.onDetach.addListener(', '// ---- tab lifecycle'), context)
  return {
    async run() { handler({ tabId: 1 }, 'target_closed'); await operation; return { timers, attaches } },
  }
}

test('discarded tabs stop detach recovery before starting retry timers (#551)', async () => {
  assert.deepEqual(await detachWorker([{ url: 'https://example.com', discarded: true }]).run(), { timers: 0, attaches: 0 })
})

test('a tab discarded during backoff is not attached; ordinary navigation still recovers', async () => {
  assert.deepEqual(await detachWorker([
    { url: 'https://example.com', discarded: false },
    { url: 'https://example.com', discarded: true },
  ]).run(), { timers: 1, attaches: 0 })
  assert.deepEqual(await detachWorker([{ url: 'https://example.com', discarded: false }]).run(), { timers: 1, attaches: 1 })
})

test('the agent cannot close a tab the user lent it (0.5.62)', async () => {
  const removed = []
  const context = {
    tabGrants: { load: async () => {}, get: tabId => (tabId === 5 ? { tabId: 5, conversationId: 'c' } : null) },
    LENT_TAB_CLOSE: 'the agent cannot close it',
    tabForTarget: targetId => Number(targetId.slice(1)),
    removeAgentTab: async tabId => { removed.push(tabId) },
    agentTabRemoval: {},
    // Ownership is read from disk before deciding how to close (#674).
    loadOwnedTabs: async () => { removed.push('owned loaded') },
    loadAgentPopups: async () => { removed.push('popups loaded') },
  }
  vm.createContext(context)
  vm.runInContext('globalThis.closeTarget = async function (params) { const method = "Target.closeTarget";\n' + section("if (method === 'Target.closeTarget') {", "if (method === 'Target.activateTarget')") + '}', context)
  await assert.rejects(context.closeTarget({ targetId: 'T5' }), /cannot close it/)
  assert.deepEqual(removed, [])
  assert.equal(JSON.stringify(await context.closeTarget({ targetId: 'T6' })), '{"success":true}')
  assert.deepEqual(removed, ['owned loaded', 'popups loaded', 6])
})

test('an unattached owned target can close through the exact live Chrome registry', async () => {
  const removed = [], guarded = []
  const context = {
    tabGrants: { load: async () => {}, get: () => null },
    LENT_TAB_CLOSE: 'the agent cannot close it', agentSession: 'task',
    tabForTarget: () => null,
    ownedTabs: new Set([7]), agentPopups: new Set(),
    loadOwnedTabs: async () => {}, loadAgentPopups: async () => {},
    chrome: { debugger: { getTargets: async () => [{ id: 'T7', tabId: 7, type: 'page' }] } },
    withRelayTimeout: p => p,
    guardTab: (id, session) => { guarded.push([id, session]) },
    removeAgentTab: async id => { removed.push(id) }, agentTabRemoval: {},
  }
  vm.createContext(context)
  vm.runInContext('globalThis.closeTarget = async function (params) { const method = "Target.closeTarget";\n' + section("if (method === 'Target.closeTarget') {", "if (method === 'Target.activateTarget')") + '}', context)
  assert.equal(JSON.stringify(await context.closeTarget({ targetId: 'T7' })), '{"success":true}')
  assert.deepEqual(removed, [7])
  assert.deepEqual(guarded, [[7, 'task']])

  for (const registry of [
    [{ id: 'OTHER', tabId: 7, type: 'page' }],
    [{ id: 'T7', tabId: 8, type: 'page' }],
    [{ id: 'T7', tabId: 7, type: 'iframe' }],
    [{ id: 'T7', tabId: 7, type: 'page' }, { id: 'T7', tabId: 9, type: 'page' }],
  ]) {
    context.chrome.debugger.getTargets = async () => registry
    removed.length = 0
    assert.equal(JSON.stringify(await context.closeTarget({ targetId: 'T7' })), '{"success":false}')
    assert.deepEqual(removed, [])
  }
  context.chrome.debugger.getTargets = async () => [{ id: 'T7', tabId: 7, type: 'page' }]
  context.tabGrants.get = () => ({ tabId: 7 })
  await assert.rejects(context.closeTarget({ targetId: 'T7' }), /cannot close it/)
  assert.deepEqual(removed, [])
  context.tabGrants.get = () => null
  context.guardTab = () => { throw new Error('session access denied') }
  await assert.rejects(context.closeTarget({ targetId: 'T7' }), /session access denied/)
  assert.deepEqual(removed, [])
})
