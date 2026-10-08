import assert from 'node:assert/strict'
import test from 'node:test'

import { agentWindowStillOurs } from './agent-window.js'

const owned = new Set([1, 2])
const isOwned = (id) => owned.has(id)

test('a window holding only agent tabs and its placeholder stays the agent window', () => {
  const got = agentWindowStillOurs(
    { id: 9, state: 'normal' },
    [
      { id: 1, url: 'https://example.com/' },
      { id: 2, url: 'https://example.org/' },
      { id: 3, url: 'about:blank' },
    ],
    isOwned,
  )
  assert.deepEqual(got, { ours: true, reason: null })
})

test('a pop-up opened by an agent tab does not disqualify the window', () => {
  const got = agentWindowStillOurs(
    { id: 9, state: 'normal' },
    [
      { id: 1, url: 'https://example.com/' },
      { id: 5, url: 'https://login.example.com/', openerTabId: 1 },
    ],
    isOwned,
  )
  assert.equal(got.ours, true)
})

test('a user tab in the remembered window means it is the user window now', () => {
  const got = agentWindowStillOurs(
    { id: 9, state: 'normal' },
    [
      { id: 1, url: 'https://example.com/' },
      { id: 7, url: 'https://mail.example.com/' },
    ],
    isOwned,
  )
  assert.deepEqual(got, { ours: false, reason: 'user-tab' })
})

test('a full-screen window is never the agent window', () => {
  const got = agentWindowStillOurs({ id: 9, state: 'fullscreen' }, [{ id: 1, url: 'https://example.com/' }], isOwned)
  assert.deepEqual(got, { ours: false, reason: 'fullscreen' })
})

test('a missing window is not ours', () => {
  assert.deepEqual(agentWindowStillOurs(null, [], isOwned), { ours: false, reason: 'gone' })
})

test('a tab still loading its first page is judged by its pending url', () => {
  const got = agentWindowStillOurs(
    { id: 9, state: 'normal' },
    [{ id: 8, url: '', pendingUrl: 'https://news.example.com/' }],
    isOwned,
  )
  assert.equal(got.ours, false)
})
