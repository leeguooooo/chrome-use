import assert from 'node:assert/strict'
import test from 'node:test'

import { targetInfoForTab } from './target-info.js'

test('targetInfoForTab resolves a hung tab from the browser-level target list', () => {
  const info = targetInfoForTab(
    [
      { id: 'worker-target', tabId: 42, type: 'worker', url: '', title: '' },
      {
        id: 'page-target',
        tabId: 42,
        type: 'page',
        url: 'https://example.com/stuck',
        title: 'Stuck page',
      },
    ],
    42,
  )

  assert.deepEqual(info, {
    targetId: 'page-target',
    type: 'page',
    url: 'https://example.com/stuck',
    title: 'Stuck page',
    attached: true,
  })
})

test('targetInfoForTab does not drift to another tab', () => {
  assert.equal(
    targetInfoForTab([{ id: 'other', tabId: 7, type: 'page' }], 42),
    null,
  )
})
