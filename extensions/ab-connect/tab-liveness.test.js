import assert from 'node:assert/strict'
import test from 'node:test'

import { reconcileAttachedTabEntries, resolveFirstLiveTab } from './tab-liveness.js'

test('inspect resolution prefers a live target mapping over a dead encoded session tab', async () => {
  const got = await resolveFirstLiveTab(
    [92, 41],
    async (tabId) => (tabId === 41 ? { id: 41, url: 'https://example.com/live' } : null),
  )

  assert.deepEqual(got, {
    tabId: 41,
    tab: { id: 41, url: 'https://example.com/live' },
  })
})

test('reannounce drops phantom tab records before publishing targets (#196)', async () => {
  const calls = []
  const entries = [
    [11, { sessionId: 'cb-tab-11', targetId: 'dead-blank' }],
    [22, { sessionId: 'cb-tab-22', targetId: 'live-page' }],
  ]
  const result = await reconcileAttachedTabEntries(entries, {
    getTab: async (tabId) => (tabId === 22 ? { id: 22 } : null),
    detach: (tabId) => calls.push(['detach', tabId]),
    unmarkOwned: (tabId) => calls.push(['unmarkOwned', tabId]),
  })

  assert.deepEqual(result.live, [[22, entries[1][1]]])
  assert.deepEqual(result.removed, [
    { tabId: 11, targetId: 'dead-blank', sessionId: 'cb-tab-11' },
  ])
  assert.deepEqual(calls, [
    ['detach', 11],
    ['unmarkOwned', 11],
  ])
})
