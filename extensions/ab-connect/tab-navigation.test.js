import assert from 'node:assert/strict'
import test from 'node:test'

import {
  canUseBrowserNavigationFallback,
  navigateTabWithBrowserFallback,
} from './tab-navigation.js'

function fixture(overrides = {}) {
  const calls = []
  const deps = {
    navigateWithDebugger: async () => {
      calls.push(['navigateWithDebugger'])
      return { frameId: 'frame-1', loaderId: 'loader-1' }
    },
    updateTab: async (url) => {
      calls.push(['updateTab', url])
      return { pendingUrl: url, url: 'https://example.com/old', title: 'Old title' }
    },
    ...overrides,
  }
  return { calls, deps }
}

test('top-level navigation uses the ordinary browser tab path first (#211)', async () => {
  const { calls, deps } = fixture()
  const result = await navigateTabWithBrowserFallback({ url: 'https://example.com' }, deps)

  assert.deepEqual(calls, [['updateTab', 'https://example.com']])
  assert.deepEqual(result, {
    frameId: '',
    loaderId: 'browser-level-navigation',
    relayFallback: {
      method: 'browser-level chrome.tabs.update',
      recovered: false,
      url: 'https://example.com',
      title: '',
    },
  })
})

test('browser fallback is limited to top-level navigation', () => {
  assert.equal(canUseBrowserNavigationFallback('Page.navigate', {}, null), true)
  assert.equal(
    canUseBrowserNavigationFallback('Page.navigate', { frameId: 'child-frame' }, null),
    false,
  )
  assert.equal(canUseBrowserNavigationFallback('Page.navigate', {}, 'child-session'), false)
  assert.equal(canUseBrowserNavigationFallback('Runtime.evaluate', {}, null), false)
})

test('browser API rejection falls back to Page.navigate', async () => {
  const { calls, deps } = fixture({
    updateTab: async () => {
      calls.push(['updateTab'])
      throw new Error('browser API rejected URL')
    },
  })

  const result = await navigateTabWithBrowserFallback({ url: 'file:///tmp/test.html' }, deps)
  assert.equal(result.loaderId, 'loader-1')
  assert.deepEqual(calls, [['updateTab'], ['navigateWithDebugger']])
})

test('dual navigation failure keeps both errors', async () => {
  const { deps } = fixture({
    updateTab: async () => {
      throw new Error('tab update denied')
    },
    navigateWithDebugger: async () => {
      throw new Error('tab is gone')
    },
  })

  await assert.rejects(
    navigateTabWithBrowserFallback({ url: 'https://example.com' }, deps),
    /Browser-level navigation failed: tab update denied.*CDP fallback also failed: tab is gone/,
  )
})

test('normalizeNavigationUrl prefixes schemes and preserves existing ones', async () => {
  const { calls, deps } = fixture()
  await navigateTabWithBrowserFallback({ url: 'google.com' }, deps)
  assert.equal(calls[0][1], 'https://google.com')

  const { calls: calls2, deps: deps2 } = fixture()
  await navigateTabWithBrowserFallback({ url: 'localhost:8080/app' }, deps2)
  assert.equal(calls2[0][1], 'http://localhost:8080/app')

  const { calls: calls3, deps: deps3 } = fixture()
  await navigateTabWithBrowserFallback({ url: 'http://custom.domain' }, deps3)
  assert.equal(calls3[0][1], 'http://custom.domain')
})
