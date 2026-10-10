import assert from 'node:assert/strict'
import test from 'node:test'

import { exactTabPresence, isMissingTabError, targetPresence, TAB_PRESENCE_VERSION } from './tab-presence.js'

const missing = (id) => async () => {
  throw new Error(`No tab with id: ${id}.`)
}

test('only Chrome\'s exact "No tab with id" for that id is absent', async () => {
  assert.deepEqual(await exactTabPresence(41, missing(41)), {
    tabPresenceVersion: TAB_PRESENCE_VERSION,
    tabId: 41,
    presence: 'absent',
  })
  assert.equal(isMissingTabError(new Error('No tab with id: 41'), 41), true)
  assert.equal(isMissingTabError('No tab with id: 41.', 41), true)
})

test('an API error is not a missing tab', async () => {
  const errors = [
    'Tabs cannot be edited right now (user may be dragging a tab).',
    'Cannot access contents of the page. Extension manifest must request permission to access the respective host.',
    'Extension context invalidated.',
    'relay timeout: chrome.tabs.get did not answer within 8000ms',
    'No tab with id: 410.', // another tab
    'No tab with id: 4.', // a prefix of the id
    'Error: No tab with id: 41. (wrapped)',
    '',
  ]
  for (const message of errors) {
    const got = await exactTabPresence(41, async () => {
      throw new Error(message)
    })
    assert.equal(got.presence, 'unknown', message)
    assert.equal(got.tabPresenceVersion, TAB_PRESENCE_VERSION)
    assert.equal(got.error, message)
  }
  for (const thrown of [undefined, null, 42, { code: 'x' }]) {
    const got = await exactTabPresence(41, async () => {
      throw thrown
    })
    assert.equal(got.presence, 'unknown', String(thrown))
  }
})

test('an open tab is present, with its url', async () => {
  const got = await exactTabPresence(7, async (id) => ({ id, url: 'https://example.com/' }))
  assert.deepEqual(got, {
    tabPresenceVersion: TAB_PRESENCE_VERSION,
    tabId: 7,
    presence: 'present',
    url: 'https://example.com/',
  })
})

test('an answer without that tab, or for another tab, is unknown', async () => {
  assert.equal((await exactTabPresence(7, async () => null)).presence, 'unknown')
  assert.equal((await exactTabPresence(7, async () => undefined)).presence, 'unknown')
  assert.equal((await exactTabPresence(7, async () => ({ id: 8 }))).presence, 'unknown')
})

test('an invalid tab id is unknown and never read', async () => {
  for (const id of [NaN, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, undefined]) {
    let read = false
    const got = await exactTabPresence(id, async () => {
      read = true
      throw new Error(`No tab with id: ${id}.`)
    })
    assert.equal(got.presence, 'unknown', String(id))
    assert.equal(read, false)
  }
})

const registry = (entries) => async () => entries
const tabs = (open, failures = {}) => async (id) => {
  if (failures[id]) throw new Error(failures[id])
  if (open[id]) return { id, url: open[id] }
  throw new Error(`No tab with id: ${id}.`)
}

test('a target gone from Chrome\'s registry and its exact tab gone is absent', async () => {
  const got = await targetPresence(
    { targetId: 'T', tabId: 5 },
    { getTargets: registry([{ id: 'OTHER', tabId: 6 }]), getTab: tabs({ 6: 'https://other/' }) },
  )
  assert.deepEqual(got, { tabPresenceVersion: TAB_PRESENCE_VERSION, targetId: 'T', tabId: 5, presence: 'absent' })
})

test('the same target under a new tab id after a replacement is present, never absent', async () => {
  // The relay recorded tab 5; Chrome replaced it (discard / prerender swap)
  // with tab 9 holding the same target, and tab 5 no longer exists.
  const got = await targetPresence(
    { targetId: 'T', tabId: 5 },
    { getTargets: registry([{ id: 'T', tabId: 9 }]), getTab: tabs({ 9: 'https://kept/' }) },
  )
  assert.equal(got.presence, 'present')
  assert.equal(got.tabId, 9)
  assert.equal(got.url, 'https://kept/')
})

test('an API error is never absence, at any step', async () => {
  const getTargetsFails = await targetPresence(
    { targetId: 'T', tabId: 5 },
    {
      getTargets: async () => {
        throw new Error('Extension context invalidated.')
      },
      getTab: tabs({}),
    },
  )
  assert.equal(getTargetsFails.presence, 'unknown')
  const tabReadFails = await targetPresence(
    { targetId: 'T', tabId: 5 },
    { getTargets: registry([]), getTab: tabs({}, { 5: 'Tabs cannot be edited right now (user may be dragging a tab).' }) },
  )
  assert.equal(tabReadFails.presence, 'unknown')
  assert.match(tabReadFails.error, /dragging/)
  const listedTabFails = await targetPresence(
    { targetId: 'T', tabId: 5 },
    { getTargets: registry([{ id: 'T', tabId: 9 }]), getTab: tabs({}, { 9: 'boom' }) },
  )
  assert.equal(listedTabFails.presence, 'unknown')
  const notAList = await targetPresence({ targetId: 'T', tabId: 5 }, { getTargets: async () => null, getTab: tabs({}) })
  assert.equal(notAList.presence, 'unknown')
})

test('insufficient identity is unknown', async () => {
  // No target id: nothing to look up.
  assert.equal((await targetPresence({ tabId: 5 }, { getTargets: registry([]), getTab: tabs({}) })).presence, 'unknown')
  // Not listed and no tab id to confirm the absence with.
  assert.equal(
    (await targetPresence({ targetId: 'T' }, { getTargets: registry([]), getTab: tabs({}) })).presence,
    'unknown',
  )
  // The recorded tab still exists but no longer lists the target.
  const moved = await targetPresence(
    { targetId: 'T', tabId: 5 },
    { getTargets: registry([{ id: 'NEW', tabId: 5 }]), getTab: tabs({ 5: 'https://x/' }) },
  )
  assert.equal(moved.presence, 'unknown')
  // A malformed tab id is never read as a missing tab.
  assert.equal(
    (await targetPresence({ targetId: 'T', tabId: Number('x') }, { getTargets: registry([]), getTab: tabs({}) })).presence,
    'unknown',
  )
})

test('a listed target in an open tab is present', async () => {
  const got = await targetPresence(
    { targetId: 'T', tabId: 5 },
    { getTargets: registry([{ id: 'T', tabId: 5 }]), getTab: tabs({ 5: 'https://open/' }) },
  )
  assert.deepEqual(got, {
    tabPresenceVersion: TAB_PRESENCE_VERSION,
    targetId: 'T',
    tabId: 5,
    presence: 'present',
    url: 'https://open/',
  })
})

test('a malformed target list is never evidence that a target is gone', async () => {
  const lists = [
    [null],
    [{ id: 7 }],
    [{ tabId: 5 }],
    [{ id: '' }],
    [undefined],
    ['T'],
    [{ id: 'OTHER', tabId: 6 }, null],
    [{ id: 'OTHER', tabId: 6 }, { id: 'X', tabId: -1 }],
    [{ id: 'OTHER', tabId: 6 }, { id: 'X', tabId: '5' }],
    // Duplicate / contradictory ids.
    [{ id: 'OTHER', tabId: 6 }, { id: 'OTHER', tabId: 7 }],
    [{ id: 'T', tabId: 5 }, { id: 'T', tabId: 9 }],
  ]
  for (const list of lists) {
    let read = false
    const got = await targetPresence(
      { targetId: 'T', tabId: 5 },
      {
        getTargets: registry(list),
        getTab: async (id) => {
          read = true
          throw new Error(`No tab with id: ${id}.`)
        },
      },
    )
    assert.equal(got.presence, 'unknown', JSON.stringify(list))
    assert.equal(read, false, `read a tab on the strength of ${JSON.stringify(list)}`)
  }
})

test('a replacement during the old tab read: the target found in a new tab is present', async () => {
  // First list: T is not there (mid-replacement). While tab 5 is read (and
  // reported missing), T appears in tab 9. Not the static case above, where
  // the first list already has the new tab.
  let reads = 0
  const lists = [[{ id: 'OTHER', tabId: 6 }], [{ id: 'OTHER', tabId: 6 }, { id: 'T', tabId: 9 }]]
  const got = await targetPresence(
    { targetId: 'T', tabId: 5 },
    {
      getTargets: async () => lists[Math.min(reads++, lists.length - 1)],
      getTab: tabs({ 9: 'https://moved/' }),
    },
  )
  assert.equal(reads, 2, 'the registry was read again after the missing tab')
  assert.equal(got.presence, 'present')
  assert.equal(got.tabId, 9)
})

test('the registry unreadable or malformed after the missing tab is unknown, not absent', async () => {
  let reads = 0
  const got = await targetPresence(
    { targetId: 'T', tabId: 5 },
    {
      getTargets: async () => {
        if (reads++ === 0) return [{ id: 'OTHER', tabId: 6 }]
        throw new Error('relay timeout: chrome.debugger.getTargets did not answer within 8000ms')
      },
      getTab: tabs({}),
    },
  )
  assert.equal(got.presence, 'unknown')
  reads = 0
  const malformed = await targetPresence(
    { targetId: 'T', tabId: 5 },
    {
      getTargets: async () => (reads++ === 0 ? [{ id: 'OTHER', tabId: 6 }] : [null]),
      getTab: tabs({}),
    },
  )
  assert.equal(malformed.presence, 'unknown')
})

test('absent needs both registry reads to leave the target out', async () => {
  let reads = 0
  const got = await targetPresence(
    { targetId: 'T', tabId: 5 },
    {
      getTargets: async () => {
        reads++
        return [{ id: 'OTHER', tabId: 6 }]
      },
      getTab: tabs({ 6: 'https://other/' }),
    },
  )
  assert.equal(got.presence, 'absent')
  assert.equal(reads, 2)
})

test('a target the registry was read without is reported unlisted, for the CLI to drop (#519)', async () => {
  const unlisted = await targetPresence(
    { targetId: 'PHANTOM' },
    { getTargets: registry([{ id: 'T', tabId: 5 }]), getTab: tabs({ 5: 'https://x/' }) },
  )
  assert.equal(unlisted.presence, 'unknown')
  assert.equal(unlisted.listed, false)
  const moved = await targetPresence(
    { targetId: 'OLD', tabId: 5 },
    { getTargets: registry([{ id: 'NEW', tabId: 5 }]), getTab: tabs({ 5: 'https://x/' }) },
  )
  assert.equal(moved.listed, false)
  // An unreadable registry says nothing about listing.
  const unread = await targetPresence(
    { targetId: 'T' },
    { getTargets: async () => { throw new Error('boom') }, getTab: tabs({}) },
  )
  assert.equal(unread.presence, 'unknown')
  assert.equal('listed' in unread, false)
})
