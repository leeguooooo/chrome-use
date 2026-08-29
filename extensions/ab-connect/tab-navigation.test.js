import assert from 'node:assert/strict'
import test from 'node:test'

import { navigateTabWithBrowserFallback } from './tab-navigation.js'

function timeoutError() {
  const error = new Error('relay timeout after 8000ms: Page.navigate')
  error.name = 'RelayTimeoutError'
  return error
}

function fixture(overrides = {}) {
  const calls = []
  const deps = {
    navigateWithDebugger: async () => ({ frameId: 'frame-1', loaderId: 'loader-1' }),
    isRelayTimeoutError: (error) => error?.name === 'RelayTimeoutError',
    updateTab: async (url) => calls.push(['updateTab', url]),
    ...overrides,
  }
  return { calls, deps }
}

test('returns the ordinary debugger navigation result', async () => {
  const { calls, deps } = fixture()
  const result = await navigateTabWithBrowserFallback({ url: 'https://example.com' }, deps)

  assert.equal(result.loaderId, 'loader-1')
  assert.deepEqual(calls, [])
})

test('renderer timeout recovers through browser-level navigation (#193)', async () => {
  const { calls, deps } = fixture({
    navigateWithDebugger: async () => {
      throw timeoutError()
    },
  })
  const result = await navigateTabWithBrowserFallback({ url: 'https://example.com/heavy' }, deps)

  assert.deepEqual(calls, [['updateTab', 'https://example.com/heavy']])
  assert.deepEqual(result, {
    frameId: '',
    loaderId: null,
    relayFallback: 'browser-level chrome.tabs.update',
  })
})

test('unrelated debugger errors are not hidden by the fallback', async () => {
  const { calls, deps } = fixture({
    navigateWithDebugger: async () => {
      throw new Error('permission denied')
    },
  })

  await assert.rejects(
    navigateTabWithBrowserFallback({ url: 'https://example.com' }, deps),
    /permission denied/,
  )
  assert.deepEqual(calls, [])
})

test('fallback failure keeps both errors', async () => {
  const { deps } = fixture({
    navigateWithDebugger: async () => {
      throw timeoutError()
    },
    updateTab: async () => {
      throw new Error('tab is gone')
    },
  })

  await assert.rejects(
    navigateTabWithBrowserFallback({ url: 'https://example.com' }, deps),
    /relay timeout.*fallback also failed: tab is gone/,
  )
})
