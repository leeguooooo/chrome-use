import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'

import { sendTabCommand } from './tab-command.js'

function fixture(error, recoveredTab = 42) {
  const sent = []
  const detached = []
  const recovered = []
  return {
    sent, detached, recovered,
    deps: {
      async sendCommand(target, method, params) {
        sent.push({ target, method, params })
        if (sent.length === 1) throw error
        return { frameTree: { frame: { url: 'https://example.com/after' } } }
      },
      detachTab(...args) { detached.push(args) },
      async recoverSessionTab(session) { recovered.push(session); return recoveredTab },
    },
  }
}

test('top-level read follows the recovered tab rather than retrying the dead tab', async () => {
  const f = fixture(new Error('Target closed'))
  const result = await sendTabCommand(7, 'Page.getFrameTree', undefined, undefined, f.deps)
  assert.equal(result.frameTree.frame.url, 'https://example.com/after')
  assert.deepEqual(f.sent.map(x => x.target), [{ tabId: 7 }, { tabId: 42 }])
  assert.deepEqual(f.recovered, ['cb-tab-7'])
})

test('a restricted child frame does not detach or rebind its healthy parent tab', async () => {
  const error = new Error('Cannot access a chrome-extension:// URL of different extension')
  const f = fixture(error)
  await assert.rejects(
    sendTabCommand(7, 'Accessibility.getFullAXTree', {}, 'child-frame', f.deps),
    /debugger_access_denied:/,
  )
  assert.equal(f.sent.length, 1)
  assert.deepEqual(f.sent[0].target, { tabId: 7, sessionId: 'child-frame' })
  assert.deepEqual(f.detached, [])
  assert.deepEqual(f.recovered, [])
})

test('a detached child session is not replayed after reattaching its parent', async () => {
  const error = new Error('Session detached')
  const f = fixture(error)
  await assert.rejects(
    sendTabCommand(7, 'Runtime.evaluate', { expression: 'document.title' }, 'child-frame', f.deps),
    e => e === error,
  )
  assert.equal(f.sent.length, 1)
  assert.deepEqual(f.detached, [])
})

test('failed top-level recovery preserves the original error without another dispatch', async () => {
  const error = new Error('Target closed')
  const f = fixture(error, null)
  await assert.rejects(sendTabCommand(7, 'Page.getFrameTree', {}, undefined, f.deps), e => e === error)
  assert.equal(f.sent.length, 1)
})

test('ordinary page errors do not trigger attachment recovery', async () => {
  const error = new Error('Invalid parameters')
  const f = fixture(error)
  await assert.rejects(sendTabCommand(7, 'DOM.resolveNode', {}, undefined, f.deps), e => e === error)
  assert.deepEqual(f.detached, [])
  assert.deepEqual(f.recovered, [])
})

test('parent observation still works after a child permission error', async () => {
  const error = new Error('Cannot access a chrome-extension:// URL of different extension')
  const f = fixture(error)
  await assert.rejects(sendTabCommand(7, 'DOM.getDocument', {}, 'child-frame', f.deps))
  await sendTabCommand(7, 'Page.getFrameTree', {}, undefined, f.deps)
  assert.deepEqual(f.sent.map(x => x.target), [
    { tabId: 7, sessionId: 'child-frame' }, { tabId: 7 },
  ])
  assert.deepEqual(f.recovered, [])
})

test('relay timeout does not replay a possibly completed action', async () => {
  const error = new Error('relay timeout: Runtime.evaluate')
  error.name = 'RelayTimeoutError'
  const f = fixture(error)
  await assert.rejects(sendTabCommand(7, 'Runtime.evaluate', {}, undefined, f.deps), e => e === error)
  assert.equal(f.sent.length, 1)
  assert.deepEqual(f.detached, [])
})

test('recovery is bounded when the replacement also fails', async () => {
  let attempts = 0
  let recoveries = 0
  const error = new Error('Target closed')
  await assert.rejects(sendTabCommand(7, 'Page.getFrameTree', {}, undefined, {
    async sendCommand() { attempts++; throw error },
    detachTab() {},
    async recoverSessionTab() { recoveries++; return 42 },
  }), e => e === error)
  assert.equal(attempts, 2)
  assert.equal(recoveries, 1)
})

test('an action that changes the page before detaching is not executed twice', async () => {
  let submissions = 0
  let recoveryCalls = 0
  await assert.rejects(sendTabCommand(7, 'Runtime.evaluate', {
    expression: 'document.querySelector("form").requestSubmit()',
  }, undefined, {
    async sendCommand() {
      submissions++
      throw new Error('Detached while handling command')
    },
    detachTab() { recoveryCalls++ },
    async recoverSessionTab() { recoveryCalls++; return 7 },
  }), /action_outcome_unknown:.*not replayed/)
  assert.equal(submissions, 1)
  assert.equal(recoveryCalls, 0)
})

test('explicit rejection before dispatch can reattach and execute an action once', async () => {
  const f = fixture(new Error('Debugger is not attached to the tab with id: 7.'), 7)
  await sendTabCommand(7, 'Input.insertText', { text: 'fixture' }, undefined, f.deps)
  assert.equal(f.sent.length, 2)
  assert.deepEqual(f.recovered, ['cb-tab-7'])
})

test('a lost reply after pre-dispatch recovery still forbids another action retry', async () => {
  let attempts = 0
  let submissions = 0
  await assert.rejects(sendTabCommand(7, 'Runtime.evaluate', {}, undefined, {
    async sendCommand() {
      attempts++
      if (attempts === 1) throw new Error('Debugger is not attached to the tab with id: 7.')
      submissions++
      throw new Error('Detached while handling command')
    },
    detachTab() {},
    async recoverSessionTab() { return 7 },
  }), /action_outcome_unknown:/)
  assert.equal(attempts, 2)
  assert.equal(submissions, 1)
})


test('protected content in a top-level tab does not trigger futile reattachment', async () => {
  const f = fixture(new Error('Cannot access a chrome-extension:// URL of different extension'))
  await assert.rejects(sendTabCommand(7, 'Page.getFrameTree', {}, undefined, f.deps), /debugger_access_denied:/)
  assert.equal(f.sent.length, 1)
  assert.deepEqual(f.detached, [])
  assert.deepEqual(f.recovered, [])
})

async function recoveryFixture() {
  const { createAttachmentHealth } = await import('./tab-command.js')
  const calls = []
  let hung = true
  const deps = {
    health: createAttachmentHealth(), commandTimeoutMs: 5, recoveryTimeoutMs: 15,
    sendCommand(target, method) {
      calls.push(['send', target.tabId, method])
      return hung && target.tabId === 1 ? new Promise(() => {}) : Promise.resolve({ ok: true })
    },
    async detachDebugger(id) { calls.push(['detach', id]); hung = false },
    detachTab(id) { calls.push(['forget', id]) },
    async attachTab(id) { calls.push(['attach', id]) },
  }
  return { calls, deps }
}

test('hung tab A does not block B; next concurrent reads share acknowledged recovery', async () => {
  const { calls, deps } = await recoveryFixture()
  const timeout = assert.rejects(sendTabCommand(1, 'Page.enable', {}, undefined, deps), /relay timeout/)
  assert.deepEqual(await sendTabCommand(2, 'Page.enable', {}, undefined, deps), { ok: true })
  await timeout
  await Promise.all([1, 2].map(() => sendTabCommand(1, 'DOM.getDocument', {}, undefined, deps)))
  assert.equal(calls.filter(c => c[0] === 'detach').length, 1)
  assert.equal(calls.filter(c => c[0] === 'attach').length, 1)
  assert.ok(calls.findIndex(c => c[0] === 'detach') < calls.findIndex(c => c[0] === 'attach'))
})

test('timed-out side effect is not replayed and next action runs once after recovery', async () => {
  const { calls, deps } = await recoveryFixture()
  await assert.rejects(sendTabCommand(1, 'Runtime.evaluate', {}, undefined, deps), /relay timeout/)
  assert.equal(calls.length, 1)
  await sendTabCommand(1, 'Input.dispatchMouseEvent', {}, undefined, deps)
  assert.equal(calls.filter(c => c[2] === 'Runtime.evaluate').length, 1)
  assert.equal(calls.filter(c => c[2] === 'Input.dispatchMouseEvent').length, 1)
})

test('recovery deadline reports an unconfirmed tab reset without dispatch', async () => {
  const { calls, deps } = await recoveryFixture()
  await assert.rejects(sendTabCommand(1, 'Page.enable', {}, undefined, deps), /relay timeout/)
  deps.detachDebugger = () => new Promise(() => {})
  await assert.rejects(sendTabCommand(1, 'DOM.getDocument', {}, undefined, deps), /tab_reset_failed: tab 1 was reset; debugger recovery could not be confirmed/)
  assert.equal(calls.length, 1)
})

test('child timeout invalidates parent and stale child is never replayed', async () => {
  const { calls, deps } = await recoveryFixture()
  await assert.rejects(sendTabCommand(1, 'Accessibility.getFullAXTree', {}, 'child', deps), /relay timeout/)
  await assert.rejects(sendTabCommand(1, 'Accessibility.getFullAXTree', {}, 'child', deps), /tab_reset: child session invalidated/)
  assert.equal(calls.filter(c => c[0] === 'send').length, 1)
  await sendTabCommand(1, 'Page.enable', {}, undefined, deps)
})

test('late detach acknowledgement after recovery deadline cannot reattach or dispatch', async () => {
  const { calls, deps } = await recoveryFixture()
  let finish
  deps.detachDebugger = () => new Promise(resolve => { finish = resolve })
  deps.health.mark(1)
  await assert.rejects(sendTabCommand(1, 'Page.enable', {}, undefined, deps), /tab_reset_failed/)
  finish()
  await new Promise(resolve => setTimeout(resolve, 0))
  await assert.rejects(sendTabCommand(1, 'Page.enable', {}, undefined, deps), /tab_reset_failed/)
  assert.deepEqual(calls, [])
})

for (const message of ['Debugger is not attached to the tab with id: 1.', 'Debugger is not attached to tab 1.']) {
  test(`already detached recovery dispatches: ${message}`, async () => {
    const { calls, deps } = await recoveryFixture()
    await assert.rejects(sendTabCommand(1, 'Page.enable', {}, undefined, deps), /relay timeout/)
    const detach = deps.detachDebugger
    deps.detachDebugger = async id => { await detach(id); throw new Error(message) }
    assert.deepEqual(await sendTabCommand(1, 'Page.enable', {}, undefined, deps), { ok: true })
    assert.deepEqual(calls.map(c => c[0]), ['send', 'detach', 'forget', 'attach', 'send'])
    assert.equal(deps.health.isRecovering(1), false)
  })
}

for (const message of ['Permission denied', 'debugger recovery refused: tab is no longer authorized', 'Target closed']) {
  test(`other detach errors fail recovery: ${message}`, async () => {
    const { calls, deps } = await recoveryFixture()
    deps.health.mark(1)
    deps.detachDebugger = async () => { throw new Error(message) }
    await assert.rejects(sendTabCommand(1, 'Page.enable', {}, undefined, deps), /tab_reset_failed/)
    assert.equal(deps.health.isRecovering(1), false)
    await assert.rejects(deps.health.recover(1, deps), /tab_reset_failed/)
    assert.deepEqual(calls, [])
    deps.health.clear(1)
    assert.equal(await deps.health.recover(1, deps), false)
  })
}

test('clearing an unhealthy tab avoids recovery', async () => {
  const { calls, deps } = await recoveryFixture()
  deps.health.mark(1)
  deps.health.clear(1)
  assert.equal(await deps.health.recover(1, deps), false)
  assert.deepEqual(calls, [])
})

for (const stage of ['detach', 'attach']) {
  for (const rejects of [false, true]) {
    test(`removal invalidates late ${stage} ${rejects ? 'failure' : 'success'}`, async () => {
      const { deps } = await recoveryFixture()
      let finish, isActive
      deps[stage === 'detach' ? 'detachDebugger' : 'attachTab'] = (_id, active) => {
        isActive = active
        return new Promise((resolve, reject) => { finish = () => rejects ? reject(new Error('tab gone')) : resolve() })
      }
      deps.health.mark(1)
      const pending = assert.rejects(deps.health.recover(1, deps), /tab_reset_failed/)
      await new Promise(resolve => setImmediate(resolve))
      assert.equal(deps.health.isRecovering(1), true)
      deps.health.clear(1)
      if (isActive) assert.equal(isActive(), false)
      finish()
      await pending
      assert.equal(deps.health.isRecovering(1), false)
      assert.equal(await deps.health.recover(1, deps), false)
    })
  }
}

// Exercise the actual background handlers without starting Chrome or the worker.
async function backgroundCleanupFixture() {
  const { createAttachmentHealth } = await import('./tab-command.js')
  const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8')
  const listeners = {}, events = []
  let pending
  const context = {
    attachmentHealth: createAttachmentHealth(),
    tabs: new Map([[1, { sessionId: 'cb-tab-1', attached: true }]]),
    sessionToTab: new Map([['cb-tab-1', 1]]),
    childSessionToTab: new Map([['child', 1]]),
    forgetSessionTab(map, id) { for (const [sid, tid] of map) if (tid === id) map.delete(sid) },
    postToHost(event) { events.push(event) },
    whenReady(fn) { pending = Promise.resolve().then(fn); return pending },
    chrome: {
      debugger: { onDetach: { addListener(fn) { listeners.detach = fn } } },
      tabs: { onRemoved: { addListener(fn) { listeners.remove = fn } } },
    },
    nativeDuplicateTabs: new Set(), forgetAgentPopup() {},
    reloadStates: new Map(), sessionTargets: new Map(),
    ownedTabs: new Set(), unmarkOwned() {},
    setTimeout() {}, RELOAD_LOOP_WINDOW_MS: 1000,
    port: null,
  }
  vm.createContext(context)
  for (const [start, end] of [
    ['function detachTab(', 'function eligible('],
    ['chrome.debugger.onDetach.addListener(', '// ---- tab lifecycle'],
    ['chrome.tabs.onRemoved.addListener(', '// `tabs.onUpdated`'],
  ]) vm.runInContext(source.slice(source.indexOf(start), source.indexOf(end, source.indexOf(start))), context)
  return { context, events, async emit(name, ...args) { listeners[name](...args); await pending } }
}

test('failed recovery does not suppress actual detach cleanup and daemon notification', async () => {
  const { context: c, events, emit } = await backgroundCleanupFixture()
  c.attachmentHealth.mark(1)
  await assert.rejects(c.attachmentHealth.recover(1, {
    detachDebugger: async () => { throw new Error('Permission denied') },
  }), /tab_reset_failed/)
  await emit('detach', { tabId: 1 }, 'replaced_with_devtools')
  assert.equal(c.tabs.size, 0)
  assert.equal(c.sessionToTab.size, 0)
  assert.equal(c.childSessionToTab.size, 0)
  assert.equal(events[0].params.method, 'Target.detachedFromTarget')
  assert.equal(await c.attachmentHealth.recover(1, {}), false)
})

for (const state of ['unhealthy', 'failed', 'pending']) {
  test(`actual tab removal clears ${state} health even without an attachment record`, async () => {
    const { context: c, emit } = await backgroundCleanupFixture()
    c.tabs.delete(1)
    c.attachmentHealth.mark(1)
    let finish, pending
    if (state === 'failed') await assert.rejects(c.attachmentHealth.recover(1, {
      detachDebugger: async () => { throw new Error('Permission denied') },
    }), /tab_reset_failed/)
    if (state === 'pending') pending = assert.rejects(c.attachmentHealth.recover(1, {
      detachDebugger: () => new Promise(resolve => { finish = resolve }),
    }), /tab_reset_failed/)
    await emit('remove', 1)
    if (finish) { finish(); await pending }
    assert.equal(c.attachmentHealth.isRecovering(1), false)
    assert.equal(await c.attachmentHealth.recover(1, {}), false)
  })
}
