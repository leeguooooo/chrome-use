import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'node:fs'
import test from 'node:test'
import { UNFORWARDED_EVENTS, shouldForwardEvent } from './cdp-event-filter.js'

test('the high-frequency events nobody reads are not forwarded', () => {
  for (const method of UNFORWARDED_EVENTS) {
    assert.equal(shouldForwardEvent(method), false, method)
  }
})

test('events the daemon depends on are forwarded', () => {
  // Target lifecycle routes sessions; Page/Runtime drive navigation waits and
  // execution contexts; the Network events below feed request tracking, HAR
  // and the adaptive settle (#228).
  for (const method of [
    'Target.attachedToTarget',
    'Target.detachedFromTarget',
    'Page.loadEventFired',
    'Page.frameNavigated',
    'Runtime.executionContextCreated',
    'Runtime.consoleAPICalled',
    'Runtime.exceptionThrown',
    'Network.requestWillBeSent',
    'Network.responseReceived',
    'Network.loadingFinished',
    'Network.loadingFailed',
  ]) {
    assert.equal(shouldForwardEvent(method), true, method)
  }
})

// The list is only safe while nothing reads what it drops, and the daemon would
// get no error if something started to. So check the Rust sources directly: no
// dropped method may appear there as a string literal.
test('no dropped event is referenced by the CLI', () => {
  const root = new URL('../../cli/src/', import.meta.url)
  const text = readdirSync(root, { recursive: true })
    .filter((path) => String(path).endsWith('.rs'))
    .map((path) => readFileSync(new URL(String(path), root), 'utf8'))
    .join('\n')
  assert.ok(text.length > 0, 'the CLI sources should be readable from the repo')
  for (const method of UNFORWARDED_EVENTS) {
    assert.ok(!text.includes(`"${method}"`), `${method} is read by the CLI; it must be forwarded`)
  }
})
