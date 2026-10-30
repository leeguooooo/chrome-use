import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import vm from 'node:vm'
import { isMissingTabError } from './tab-presence.js'
import { followReplacement, forgetReplacement } from './tab-replacement.js'
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


function replacementFixture({ replacement = 8, denied = false, discarded = false } = {}) {
  const f = fixture('missing')
  Object.assign(f, {
    isMissingTabError, followReplacement, forgetReplacement,
    replacedTabs: new Map(replacement ? [[7, replacement]] : []), sessionTargets: new Map(),
    loadReplacedTabs: async () => {}, saveReplacedTabs() {},
    isRelayTimeoutError: () => false, isDebuggerAccessDenied: () => false,
    isPermanentAttachError: () => false, transferReloadState() {},
    reportTabIfGone: async id => f.calls.push(['gone', id]),
    attachTab: async id => { f.calls.push(['attach', id]); f.tabs.set(id, { attached: true, inflight: 0 }) },
    reviveDiscardedTab: async (api, id) => { f.calls.push(['reload', id]); return true },
    guardTab: id => { if (denied && id === replacement) throw new Error('taken back replacement') },
  })
  f.chrome.tabs.get = async id => {
    f.calls.push(['get', id])
    if (id === 7) throw new Error('No tab with id: 7.')
    return { id, url: 'https://fixture.invalid/replaced', discarded }
  }
  vm.runInContext(part('async function recoverSessionTab(', '// Send a CDP command to a tab'), f)
  return f
}

test('explicit navigation follows only the Chrome-recorded replacement of a missing encoded tab', async () => {
  const f = replacementFixture()
  await f.forward('Page.navigate', { url: 'https://fixture.invalid/next' }, 'cb-tab-7', 'session')
  assert.equal(f.sessionToTab.get('cb-tab-7'), 8)
  assert.ok(f.calls.some(c => Array.isArray(c) && c[0] === 'navigate' && c[1] === 8))
  assert.ok(!f.calls.some(c => Array.isArray(c) && c[0] === 'navigate' && c[1] !== 8))
})

test('closed encoded tab reports absence and never navigates an arbitrary tab', async () => {
  const f = replacementFixture({ replacement: null })
  await assert.rejects(f.forward('Page.navigate', { url: 'https://fixture.invalid/next' }, 'cb-tab-7', 'session'), /stale sessionId/)
  assert.ok(f.calls.some(c => Array.isArray(c) && c[0] === 'gone' && c[1] === 7))
  assert.ok(!f.calls.some(c => Array.isArray(c) && ['attach', 'navigate'].includes(c[0])))
})

test('replacement ownership is checked again before navigating', async () => {
  for (const discarded of [false, true]) {
    const f = replacementFixture({ denied: true, discarded })
    await assert.rejects(f.forward('Page.navigate', { url: 'https://fixture.invalid/next' }, 'cb-tab-7', 'session'), /taken back replacement/)
    assert.ok(!f.calls.some(c => Array.isArray(c) && ['attach', 'reload', 'navigate'].includes(c[0])))
  }
})

test('unknown tab lookup failures never authorize replacement recovery', async () => {
  const f = replacementFixture()
  for (const error of ['tabs permission denied', 'relay timeout', 'No tab with id: 8.']) {
    f.chrome.tabs.get = async () => { throw new Error(error) }
    await assert.rejects(f.forward('Page.navigate', { url: 'https://fixture.invalid/next' }, 'cb-tab-7', 'session'), e => e.message === error)
    assert.ok(!f.calls.some(c => Array.isArray(c) && ['attach', 'navigate'].includes(c[0])))
  }
})


test('Chrome-confirmed stable target recovery checks ownership before attachment', async () => {
  for (const denied of [false, true]) {
    const f = replacementFixture({ denied })
    f.replacedTabs.clear()
    f.sessionTargets.set('cb-tab-7', 'stable-target')
    f.reloadStates = new Map()
    f.chrome.debugger = { getTargets: async () => [{ id: 'stable-target', tabId: 8 }] }
    const navigation = f.forward('Page.navigate', { url: 'https://fixture.invalid/next' }, 'cb-tab-7', 'session')
    if (denied) {
      await assert.rejects(navigation, /taken back replacement/)
      assert.ok(!f.calls.some(c => Array.isArray(c) && ['attach', 'reload', 'navigate'].includes(c[0])))
    } else {
      await navigation
      assert.ok(f.calls.some(c => Array.isArray(c) && c[0] === 'navigate' && c[1] === 8))
    }
  }
})
