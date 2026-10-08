import assert from 'node:assert/strict'
import test from 'node:test'

import { agentWindowStillOurs } from './agent-window.js'

const owned = new Set([1, 2])
const isOwned = (id) => owned.has(id)
const win = { id: 9, state: 'normal' }

test('a window holding only agent tabs and its recorded placeholder stays the agent window', () => {
  const got = agentWindowStillOurs(
    win,
    [
      { id: 1, windowId: 9, url: 'https://example.com/' },
      { id: 2, windowId: 9, url: 'https://example.org/' },
      { id: 3, windowId: 9, url: 'about:blank' },
    ],
    isOwned,
    { windowId: 9, tabId: 3 },
  )
  assert.deepEqual(got, { ours: true, reason: null })
})

test('a pop-up opened by an agent tab does not disqualify the window', () => {
  const got = agentWindowStillOurs(
    win,
    [
      { id: 1, windowId: 9, url: 'https://example.com/' },
      { id: 5, windowId: 9, url: 'https://login.example.com/', openerTabId: 1 },
    ],
    isOwned,
  )
  assert.equal(got.ours, true)
})

test('a user tab in the remembered window means it is the user window now', () => {
  const got = agentWindowStillOurs(
    win,
    [
      { id: 1, windowId: 9, url: 'https://example.com/' },
      { id: 7, windowId: 9, url: 'https://mail.example.com/' },
    ],
    isOwned,
  )
  assert.deepEqual(got, { ours: false, reason: 'user-tab' })
})

test("the user's own blank tab is a user tab, not a placeholder", () => {
  for (const url of ['about:blank', '', undefined, 'chrome://newtab/']) {
    const got = agentWindowStillOurs(
      win,
      [
        { id: 1, windowId: 9, url: 'https://example.com/' },
        { id: 8, windowId: 9, url },
      ],
      isOwned,
      { windowId: 9, tabId: 3 },
    )
    assert.deepEqual(got, { ours: false, reason: 'user-tab' }, String(url))
  }
})

test('after a worker restart lost the placeholder record, a blank tab is not exempted by its URL', () => {
  const got = agentWindowStillOurs(win, [{ id: 3, windowId: 9, url: 'about:blank' }], isOwned, null)
  assert.deepEqual(got, { ours: false, reason: 'user-tab' })
})

test('a placeholder record for another window or another tab exempts nothing', () => {
  const tabs = [{ id: 3, windowId: 9, url: 'about:blank' }]
  assert.equal(agentWindowStillOurs(win, tabs, isOwned, { windowId: 4, tabId: 3 }).ours, false)
  assert.equal(agentWindowStillOurs(win, tabs, isOwned, { windowId: 9, tabId: 30 }).ours, false)
})

test('an unreadable tab list is unknown, not empty', () => {
  assert.deepEqual(agentWindowStillOurs(win, null, isOwned), { ours: false, reason: 'tabs-unknown' })
  assert.deepEqual(agentWindowStillOurs(win, undefined, isOwned), {
    ours: false,
    reason: 'tabs-unknown',
  })
})

test('contradictory or missing tab metadata refuses', () => {
  assert.deepEqual(
    agentWindowStillOurs(win, [{ id: 1, windowId: 4, url: 'https://example.com/' }], isOwned),
    { ours: false, reason: 'contradictory' },
  )
  assert.deepEqual(agentWindowStillOurs(win, [{ windowId: 9, url: 'https://example.com/' }], isOwned), {
    ours: false,
    reason: 'tab-unknown',
  })
  assert.deepEqual(agentWindowStillOurs(win, [null], isOwned), { ours: false, reason: 'tab-unknown' })
})

test('a full-screen window is never the agent window', () => {
  const got = agentWindowStillOurs(
    { id: 9, state: 'fullscreen' },
    [{ id: 1, windowId: 9, url: 'https://example.com/' }],
    isOwned,
  )
  assert.deepEqual(got, { ours: false, reason: 'fullscreen' })
})

test('a missing window is not ours', () => {
  assert.deepEqual(agentWindowStillOurs(null, [], isOwned), { ours: false, reason: 'gone' })
})
