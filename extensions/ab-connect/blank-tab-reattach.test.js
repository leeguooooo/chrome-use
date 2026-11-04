// #847: an agent ran `open about:blank` to reset its tab; every later command
// failed with "stale sessionId cb-tab-<N> … its tab is gone" and `open <url>`
// was refused too, while the tab was still open. The recovery paths gated on
// `eligible`, which refuses every about: page. These run the real predicates
// and the real recovery code from background.js against a tab on about:blank.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import vm from 'node:vm'
import { isAttachmentCleanup } from './tab-command.js'
import { isMissingTabError } from './tab-presence.js'
import { followReplacement, forgetReplacement } from './tab-replacement.js'
import { canUseBrowserNavigationFallback, navigateTabWithBrowserFallback } from './tab-navigation.js'

const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8')
const part = (from, to) => source.slice(source.indexOf(from), source.indexOf(to, source.indexOf(from)))
const predicates = source.match(/^const SKIP_URL = .*$/m)[0] + '\n' +
  part('function eligible(tab) {', '// Re-attach ONLY the tabs this extension owns')

function load() {
  const context = { chrome: { tabs: { get: async () => null } } }
  vm.createContext(context)
  vm.runInContext(predicates, context)
  return context
}

test('a blank tab is re-attachable but still not eligible for discovery', () => {
  const { eligible, reattachable } = load()
  for (const url of ['about:blank', 'about:blank#reset', 'ABOUT:BLANK']) {
    assert.equal(reattachable({ id: 7, url }), true, url)
    assert.equal(eligible({ id: 7, url }), false, `${url} must stay out of adoption and discovery`)
  }
  for (const url of ['chrome://settings', 'chrome-extension://other/options.html', 'about:newtab', 'devtools://x']) {
    assert.equal(reattachable({ id: 7, url }), false, url)
  }
  assert.equal(reattachable({ id: 7, url: 'https://example.com/' }), true)
  assert.equal(reattachable(null), false)
})

// The real forward path and recoverSessionTab, with tab 7 open on `url` and
// no attachment record (its debugger detached, or the worker restarted).
function relay(url, { missing = false } = {}) {
  const calls = []
  const context = {
    isAttachmentCleanup, isMissingTabError, followReplacement, forgetReplacement,
    canUseBrowserNavigationFallback, navigateTabWithBrowserFallback,
    calls, console,
    tabs: new Map(), sessionToTab: new Map(), childSessionToTab: new Map(),
    sessionTargets: new Map([['cb-tab-7', 'T7']]), replacedTabs: new Map(), reloadStates: new Map(),
    resolveSessionTab: sid => (sid === 'cb-tab-7' ? 7 : null), tabIdFromSession: sid => (sid === 'cb-tab-7' ? 7 : null),
    tabForTarget: () => null, anyConnectedTab: () => null, guardTab() {},
    attachmentHealth: { recover: async () => false, navigationStarted: () => calls.push('navigationStarted') },
    tabCommandDependencies: () => ({}), resetReloadLoop() {}, activeReloadLoop: () => null,
    reattachTab: async () => {}, rememberReplayable() {}, maybeDriveCursor() {},
    conversationWindows: { observe: async () => {} },
    withRelayTimeout: promise => promise,
    loadReplacedTabs: async () => {}, saveReplacedTabs() {}, transferReloadState() {},
    isRelayTimeoutError: () => false, isDebuggerAccessDenied: () => false,
    isPermanentAttachError: () => false,
    reportTabIfGone: async id => calls.push(['gone', id]),
    attachTab: async id => {
      calls.push(['attach', id])
      if (/^chrome/.test(url)) throw new Error('Cannot access a chrome:// URL')
      context.tabs.set(id, { attached: true, inflight: 0, lastActivity: 0 })
    },
    sendCdpToTab: async (id, method) => { calls.push(['debugger', id, method]); return { ok: true } },
    setTimeout: fn => fn(),
    chrome: {
      tabs: {
        get: async id => {
          calls.push(['get', id])
          if (missing) throw new Error(`No tab with id: ${id}.`)
          return { id, url }
        },
        update: async (id, properties) => { calls.push(['navigate', id, properties.url]); return { pendingUrl: properties.url } },
      },
      debugger: { getTargets: async () => (missing ? [] : [{ id: 'T7', tabId: 7 }]) },
    },
  }
  vm.createContext(context)
  vm.runInContext(predicates, context)
  vm.runInContext(part('async function recoverSessionTab(', '// Send a CDP command to a tab'), context)
  vm.runInContext('async function forward(method, params, sessionId, agentSession) {\n' +
    part('  // Everything else → chrome.debugger', '\nasync function dispatchToTab'), context)
  vm.runInContext(part('async function dispatchToTab(', '// ---- attach / detach'), context)
  const did = kind => calls.filter(c => Array.isArray(c) && c[0] === kind)
  return { context, calls, did }
}

test('a command on a tab reset to about:blank re-attaches the same tab instead of failing stale', async () => {
  const r = relay('about:blank')
  assert.deepEqual(await r.context.forward('Runtime.evaluate', { expression: '1' }, 'cb-tab-7', 'session'), { ok: true })
  assert.deepEqual(r.did('attach'), [['attach', 7]])
  assert.deepEqual(r.did('debugger'), [['debugger', 7, 'Runtime.evaluate']])
  assert.deepEqual(r.did('gone'), [])
})

test('recoverSessionTab returns the blank tab itself', async () => {
  const r = relay('about:blank')
  assert.equal(await r.context.recoverSessionTab('cb-tab-7'), 7)
  assert.ok(r.context.tabs.has(7))
})

test('open <url> navigates a blank, unattached tab rather than refusing it as not navigable', async () => {
  const r = relay('about:blank')
  const result = await r.context.forward('Page.navigate', { url: 'https://saas.invalid/sql' }, 'cb-tab-7', 'session')
  assert.equal(result.relayFallback.url, 'https://saas.invalid/sql')
  assert.deepEqual(r.did('navigate'), [['navigate', 7, 'https://saas.invalid/sql']])
})

test('a tab on a page extensions may not debug is reported open, not gone, and nothing is navigated', async () => {
  const r = relay('chrome://settings')
  await assert.rejects(
    r.context.forward('Runtime.evaluate', { expression: '1' }, 'cb-tab-7', 'session'),
    e => /stale sessionId cb-tab-7 for Runtime\.evaluate: its tab 7 is still open/.test(e.message) &&
      /chrome: page/.test(e.message) && !/its tab is gone/.test(e.message) && !/settings/.test(e.message),
  )
  await assert.rejects(r.context.forward('Page.navigate', { url: 'https://x.invalid/' }, 'cb-tab-7', 'session'), /not a navigable web tab/)
  assert.deepEqual(r.did('navigate'), [])
  assert.deepEqual(r.did('debugger'), [])
})

test('a closed tab still reports that its tab is gone', async () => {
  const r = relay('about:blank', { missing: true })
  await assert.rejects(r.context.forward('Runtime.evaluate', { expression: '1' }, 'cb-tab-7', 'session'), /its tab is gone/)
  assert.deepEqual(r.did('gone'), [['gone', 7]])
  assert.deepEqual(r.did('attach'), [])
})

// The detach Chrome sends when the tab leaves its page (a cross-process
// navigation is one ordinary cause) must re-attach a tab now on about:blank.
function detachWorker(url) {
  let handler, operation, attaches = 0
  const context = {
    console,
    chrome: {
      debugger: { onDetach: { addListener: fn => { handler = fn } } },
      tabs: { get: async id => ({ id, url, discarded: false }) },
    },
    whenReady: fn => { operation = fn(); return operation },
    releasedTabs: new Set(), attachmentHealth: { isRecovering: () => false },
    tabs: new Map(), nativeDuplicateTabs: new Set(), port: {},
    detachTab() {},
    attachTab: async () => { attaches++ },
    isPermanentAttachError: () => false,
    setTimeout: fn => fn(),
  }
  vm.createContext(context)
  vm.runInContext(predicates, context)
  vm.runInContext(part('chrome.debugger.onDetach.addListener(', '// ---- tab lifecycle'), context)
  return async () => { handler({ tabId: 7 }, 'target_closed'); await operation; return attaches }
}

test('a debugger detach while the tab shows about:blank re-attaches it', async () => {
  assert.equal(await detachWorker('about:blank')(), 1)
  assert.equal(await detachWorker('chrome://settings')(), 0)
})
