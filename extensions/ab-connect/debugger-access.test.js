import assert from 'node:assert/strict'
import test from 'node:test'

import { debuggerAccessError, foreignFrameExtensions } from './debugger-access.js'

const own = 'knfcmbamhjmaonkfnjhldjedeobeafmk'
const ajax = 'nhpjggchkhnlbgdfcbgpdpkifemomkpg'

// Shape taken from chrome.debugger.getTargets while Ajax Modifier's in-page
// frame blocked a tab (Chrome for Testing 1243).
const targets = [
  { type: 'worker', url: `chrome-extension://${ajax}/service_worker.js` },
  { type: 'other', url: `chrome-extension://${ajax}/iframe/index.html` },
  { type: 'other', url: `chrome-extension://${ajax}/iframe/index.html` },
  { type: 'other', url: `chrome-extension://${own}/inspect-frame.html` },
  { type: 'page', url: `chrome-extension://bpoadfkcbjbfhfodiogcnhhhpibjhbnh/options.html`, tabId: 3 },
  { type: 'page', url: 'https://chatgpt.com/', tabId: 7 },
]

test('only foreign extension frames are reported, once per extension', () => {
  assert.deepEqual(foreignFrameExtensions(targets, own), [ajax])
  assert.deepEqual(foreignFrameExtensions(null, own), [])
})

test('the access error names the candidate extensions', () => {
  const error = debuggerAccessError(new Error('Cannot access a chrome-extension:// URL of different extension'), [ajax])
  assert.match(error.message, /^debugger_access_denied:/)
  assert.match(error.message, /not confirmed blockers of this tab/)
  assert.doesNotMatch(error.message, /blocking frame belongs/)
  assert.ok(error.message.includes(`${ajax} (chrome://extensions/?id=${ajax})`))
})

test('without candidates the access error keeps its old text', () => {
  const error = debuggerAccessError(new Error('x'))
  assert.doesNotMatch(error.message, /Frames of other extensions/)
  assert.match(error.message, /Original error: x$/)
})
