import assert from 'node:assert/strict'
import test from 'node:test'

import {
  agentTabPredicate,
  agentWindowStillOurs,
  isUntouchedPlaceholder,
  isVerifiedAgentPopup,
  migratePopupRecord,
  placeholderCleanup,
} from './agent-window.js'

test('an adopted user tab (attached, not owned) inside the agent window rejects it', () => {
  const owned = new Set([1])
  const agentPopups = new Set()
  // The relay is attached to 1 (agent) and 7 (a user tab taken with adopt).
  const attached = new Set([1, 7])
  const isAgent = agentTabPredicate(owned, agentPopups)
  assert.equal(isAgent(7), false, 'attached is not agent identity')
  assert.ok(attached.has(7))
  const got = agentWindowStillOurs(
    { id: 9, state: 'normal' },
    [
      { id: 1, windowId: 9, url: 'https://example.com/' },
      { id: 7, windowId: 9, url: 'https://mail.example.com/' },
    ],
    isAgent,
  )
  assert.deepEqual(got, { ours: false, reason: 'user-tab' })
})

test('owned tabs plus a verified agent pop-up keep the agent window', () => {
  const isAgent = agentTabPredicate(new Set([1]), new Set([5]))
  const got = agentWindowStillOurs(
    { id: 9, state: 'normal' },
    [
      { id: 1, windowId: 9, url: 'https://example.com/' },
      // Chrome reported the window's front tab as opener, not ours: the
      // pop-up is accepted because it is a verified agent pop-up.
      { id: 5, windowId: 9, url: 'https://login.example.com/', openerTabId: 42 },
    ],
    isAgent,
  )
  assert.deepEqual(got, { ours: true, reason: null })
})

test("a tab opened by an adopted user tab is not the agent's", () => {
  const isAgent = agentTabPredicate(new Set([1]), new Set())
  const got = agentWindowStillOurs(
    { id: 9, state: 'normal' },
    [
      { id: 1, windowId: 9, url: 'https://example.com/' },
      { id: 8, windowId: 9, url: 'https://x.example/', openerTabId: 7 },
    ],
    isAgent,
  )
  assert.equal(got.ours, false)
})

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

const record = { windowId: 9, tabId: 3 }

test('a placeholder with a navigation pending is no longer exempt', () => {
  const tab = { id: 3, windowId: 9, url: 'about:blank', pendingUrl: 'https://mail.example.com/' }
  assert.equal(isUntouchedPlaceholder(tab, record), false)
  assert.deepEqual(agentWindowStillOurs(win, [tab], isOwned, record), { ours: false, reason: 'user-tab' })
  assert.equal(placeholderCleanup(tab, record, 9), 'forget')
})

test('create failed, the user navigated the placeholder, retry: their tab is never removed', () => {
  // 1. The window was created with placeholder 3; tabs.create for the agent tab
  //    failed, so the record stayed. 2. The user typed a URL into that tab.
  const navigated = { id: 3, windowId: 9, url: 'https://news.example.com/' }
  // 3. The retry's window check sees a user tab and rejects the window ...
  assert.deepEqual(agentWindowStillOurs(win, [navigated], isOwned, record), {
    ours: false,
    reason: 'user-tab',
  })
  // ... and the cleanup only forgets the record, it never removes the tab.
  assert.equal(placeholderCleanup(navigated, record, 9), 'forget')
})

test('a stale record after a worker restart exempts and removes nothing', () => {
  // The recorded tab is gone; another blank tab sits in the window.
  const other = { id: 4, windowId: 9, url: 'about:blank' }
  assert.deepEqual(agentWindowStillOurs(win, [other], isOwned, record), { ours: false, reason: 'user-tab' })
  assert.equal(placeholderCleanup(null, record, 9), 'forget')
  assert.equal(placeholderCleanup(other, record, 9), 'forget')
})

test('only an untouched placeholder in its own window is removed', () => {
  const untouched = { id: 3, windowId: 9, url: 'about:blank' }
  assert.equal(placeholderCleanup(untouched, record, 9), 'remove')
  assert.equal(placeholderCleanup({ ...untouched, pendingUrl: 'about:blank' }, record, 9), 'remove')
  assert.equal(placeholderCleanup({ ...untouched, windowId: 5 }, record, 9), 'forget')
  assert.equal(placeholderCleanup(untouched, record, 5), 'keep')
  assert.equal(placeholderCleanup(untouched, null, 9), 'keep')
})

test('a tab id alone does not verify an agent pop-up', () => {
  const isAgent = agentTabPredicate(new Set([1]), new Set())
  // Child of a user tab (7) taken with adopt: opener not an agent tab, no group.
  const child = { id: 33, groupId: -1, openerTabId: 7 }
  assert.equal(isVerifiedAgentPopup(child, [{ id: 1, groupId: -1 }, { id: 7, groupId: -1 }, child], isAgent), false)
  // Same, but the user's tab sits in a group with no agent tab in it.
  const grouped = { id: 34, groupId: 50, openerTabId: 7 }
  assert.equal(isVerifiedAgentPopup(grouped, [{ id: 7, groupId: 50 }, grouped], isAgent), false)
  assert.equal(isVerifiedAgentPopup(null, [], isAgent), false)
})

test('a pop-up opened by an agent tab, or in an agent tab group, is verified', () => {
  const isAgent = agentTabPredicate(new Set([1]), new Set([5]))
  assert.equal(isVerifiedAgentPopup({ id: 9, groupId: -1, openerTabId: 1 }, [], isAgent), true)
  assert.equal(isVerifiedAgentPopup({ id: 10, groupId: -1, openerTabId: 5 }, [], isAgent), true)
  // Chrome reported the window's front tab (42) as opener; the group holds tab 1.
  const popup = { id: 11, groupId: 60, openerTabId: 42 }
  assert.equal(isVerifiedAgentPopup(popup, [{ id: 1, groupId: 60 }, popup], isAgent), true)
})

test('a replaced pop-up keeps its record under the new id, also after onRemoved', () => {
  const popups = new Set([5])
  const recent = new Set()
  assert.equal(migratePopupRecord(popups, recent, 5, 6), true)
  assert.deepEqual([...popups], [6])
  // onRemoved first: the id moved to the recently-removed set.
  popups.delete(6)
  recent.add(6)
  assert.equal(migratePopupRecord(popups, recent, 6, 7), true)
  assert.deepEqual([...popups], [7])
  assert.equal(recent.has(6), false)
  // A tab that never was a pop-up is not upgraded by a replacement.
  assert.equal(migratePopupRecord(popups, recent, 8, 9), false)
  assert.equal(popups.has(9), false)
})

test("a pop-up in a window that also holds a user tab is not verified (Chrome's opener lies)", () => {
  const isAgent = agentTabPredicate(new Set([1]), new Set())
  // The user's tab 7 (taken with adopt) sits in the agent window 9 and opens a
  // child; Chrome reports the window's front tab (1, ours) as opener and puts
  // the child in tab 1's group.
  const child = { id: 33, windowId: 9, groupId: 60, openerTabId: 1 }
  const all = [
    { id: 1, windowId: 9, groupId: 60 },
    { id: 7, windowId: 9, groupId: -1 },
    child,
  ]
  assert.equal(isVerifiedAgentPopup(child, all, isAgent), false)
  // Without the user tab in that window the same metadata is trusted.
  assert.equal(isVerifiedAgentPopup(child, [all[0], child], isAgent), true)
})
