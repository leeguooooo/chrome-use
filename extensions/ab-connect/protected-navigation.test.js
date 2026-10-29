import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import vm from 'node:vm'
import { canUseBrowserNavigationFallback, navigateTabWithBrowserFallback } from './tab-navigation.js'

const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8')
const part = (from, to) => source.slice(source.indexOf(from), source.indexOf(to, source.indexOf(from)))

function fixture(failure, takenBack = false) {
  const calls = []
  const entry = { attached: failure !== 'detached', inflight: 0, lastActivity: 0 }
  const context = {
    calls, console, canUseBrowserNavigationFallback, navigateTabWithBrowserFallback,
    tabs: new Map(failure === 'missing' ? [] : [[7, entry]]), sessionToTab: new Map(), childSessionToTab: new Map(),
    resolveSessionTab: () => 7, tabIdFromSession: () => 7,
    tabForTarget: () => 7, anyConnectedTab: () => null,
    guardTab: () => { if (takenBack) throw new Error('taken back') },
    attachmentHealth: {
      recover: async () => { calls.push('recover'); if (failure === 'health') throw new Error('tab_reset_failed: protected extension') },
      navigationStarted: () => calls.push('navigationStarted'),
    },
    eligible: tab => !!tab && /^https?:/.test(tab.url),
    recoverSessionTab: async () => { throw new Error('debugger_access_denied: protected extension') },
    tabCommandDependencies: () => ({}), resetReloadLoop() {}, activeReloadLoop: () => null,
    reattachTab: async () => { calls.push('attach'); throw new Error('debugger_access_denied: protected extension') },
    rememberReplayable() {}, maybeDriveCursor() {}, conversationWindows: { observe: async () => {} },
    withRelayTimeout: promise => promise,
    sendCdpToTab: async () => { calls.push('debugger'); throw new Error('debugger_access_denied: protected extension') },
    chrome: { tabs: { get: async id => { calls.push(['get', id]); return { id, url: 'https://fixture.invalid/blocked' } }, update: async (id, properties) => { calls.push(['navigate', id, properties.url]); return { pendingUrl: properties.url } } } },
  }
  vm.createContext(context)
  vm.runInContext('async function forward(method, params, sessionId, agentSession) {\n' + part('  // Everything else → chrome.debugger', '\nasync function dispatchToTab'), context)
  vm.runInContext(part('async function dispatchToTab(', '// ---- attach / detach'), context)
  return context
}

for (const failure of ['detached', 'health', 'missing']) {
  test(`explicit navigation escapes ${failure} without debugger access`, async () => {
    const f = fixture(failure)
    const result = await f.forward('Page.navigate', { url: 'https://fixture.invalid/next' }, 'cb-tab-7', 'session')
    assert.equal(result.relayFallback.url, 'https://fixture.invalid/next')
    assert.deepEqual(f.calls, [...(failure === 'missing' ? [['get', 7]] : []), ['navigate', 7, 'https://fixture.invalid/next'], 'navigationStarted'])
  })
}

test('a taken-back tab still refuses browser-level navigation', async () => {
  const f = fixture('health', true)
  await assert.rejects(f.forward('Page.navigate', { url: 'https://fixture.invalid/next' }, 'cb-tab-7', 'session'), /taken back/)
  assert.deepEqual(f.calls, [])
})

test('referrer and child-frame navigations still require debugger access', async () => {
  const f = fixture('health')
  await assert.rejects(f.forward('Page.navigate', { url: 'https://fixture.invalid/next', referrer: 'https://fixture.invalid/' }, 'cb-tab-7', 'session'), /tab_reset_failed/)
  assert.deepEqual(f.calls, ['recover'])
})

test('accepted navigation permits a fresh recovery but still requires its acknowledgement', async () => {
  const { createAttachmentHealth } = await import('./tab-command.js')
  const health = createAttachmentHealth()
  health.mark(7)
  let blocked = true
  let attaches = 0
  const deps = {
    detachDebugger: async () => { if (blocked) throw new Error('protected extension frame') },
    detachTab() {},
    attachTab: async () => { attaches++ },
  }
  await assert.rejects(health.recover(7, deps), /tab_reset_failed/)
  blocked = false
  await assert.rejects(health.recover(7, deps), /tab_reset_failed/)
  assert.equal(attaches, 0)
  health.navigationStarted(7)
  assert.equal(await health.recover(7, deps), true)
  assert.equal(attaches, 1)
})


test('a recovery started before navigation cannot permanently cache its later failure', async () => {
  const { createAttachmentHealth } = await import('./tab-command.js')
  const health = createAttachmentHealth()
  health.mark(7)
  let rejectDetach
  let detachStarted
  const started = new Promise(resolve => { detachStarted = resolve })
  let blocked = true
  const deps = {
    detachDebugger: async () => {
      if (!blocked) return
      detachStarted()
      await new Promise((resolve, reject) => { rejectDetach = reject })
    },
    detachTab() {}, attachTab: async () => {},
  }
  const old = health.recover(7, deps)
  await started
  health.navigationStarted(7)
  rejectDetach(new Error('old protected document'))
  await assert.rejects(old, /tab_reset_failed/)
  blocked = false
  assert.equal(await health.recover(7, deps), true)
})


test('missing or non-web tab metadata never redirects navigation to another tab', async () => {
  for (const tab of [null, { id: 7, url: 'chrome-extension://other/options.html' }]) {
    const f = fixture('missing')
    f.chrome.tabs.get = async id => { f.calls.push(['get', id]); return tab }
    await assert.rejects(f.forward('Page.navigate', { url: 'https://fixture.invalid/next' }, 'cb-tab-7', 'session'), /not a navigable web tab/)
    assert.deepEqual(f.calls, [['get', 7]])
  }
})


test('a child-session navigation keeps debugger recovery and never updates its parent tab', async () => {
  const f = fixture('health')
  f.childSessionToTab.set('child-session', 7)
  f.tabIdFromSession = () => null
  await assert.rejects(f.forward('Page.navigate', { url: 'https://fixture.invalid/frame' }, 'child-session', 'session'), /tab_reset_failed/)
  assert.deepEqual(f.calls, ['recover'])
})
