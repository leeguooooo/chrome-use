import assert from 'node:assert/strict'
import test from 'node:test'

import {
  getRelayTimeoutHistory,
  isRelayTimeoutError,
  relayInFlightCount,
  withRelayTimeout,
} from './relay-timeout.js'

test('withRelayTimeout returns a completed debugger operation', async () => {
  assert.equal(await withRelayTimeout(Promise.resolve('ok'), 'Runtime.evaluate', 20), 'ok')
})

test('withRelayTimeout rejects a debugger operation that never settles', async () => {
  await assert.rejects(
    withRelayTimeout(new Promise(() => {}), 'Runtime.evaluate', 5),
    (error) => {
      assert.equal(isRelayTimeoutError(error), true)
      assert.equal(error.name, 'RelayTimeoutError')
      assert.match(error.message, /relay timeout after 5ms: Runtime\.evaluate/)
      return true
    },
  )
})

test('timeout detection ignores unrelated errors with similar messages', () => {
  assert.equal(isRelayTimeoutError(new Error('relay timeout after 5ms: unrelated')), false)
})

test('a late rejection remains handled after the timeout wins', async () => {
  let rejectOperation
  const operation = new Promise((_, reject) => {
    rejectOperation = reject
  })
  await assert.rejects(withRelayTimeout(operation, 'Page.enable', 5), /relay timeout/)
  rejectOperation(new Error('late Chrome failure'))
  await new Promise((resolve) => setTimeout(resolve, 0))
})

test('a timeout reports the context it failed in (#193)', async () => {
  const before = getRelayTimeoutHistory().length
  // A second command sits in flight for the whole window, so the timing-out one
  // must report company rather than looking like a lone blocked renderer.
  const parallel = withRelayTimeout(new Promise(() => {}), 'Page.navigate', 200)
  await assert.rejects(
    withRelayTimeout(new Promise(() => {}), 'Runtime.evaluate', 20),
    (error) => {
      const diag = error.relayDiagnostics
      assert.equal(diag.label, 'Runtime.evaluate')
      assert.ok(diag.elapsedMs >= 20)
      assert.equal(diag.inFlight, 2)
      assert.ok(diag.oldestInFlightMs >= diag.elapsedMs)
      // The worker cannot be younger than the command it timed out: a pending
      // timer dies with its context, so an eviction mid-command never surfaces
      // as this error at all.
      assert.ok(diag.workerAgeMs >= diag.elapsedMs)
      assert.match(error.message, /\[diag in-flight=2 oldest-in-flight=\d+ms worker-age=\d+ms\]/)
      return true
    },
  )
  assert.equal(getRelayTimeoutHistory().length, before + 1)
  await assert.rejects(parallel, /relay timeout/)
})

test('a settled command stops counting as in flight', async () => {
  await withRelayTimeout(Promise.resolve('ok'), 'Runtime.evaluate', 50)
  assert.equal(relayInFlightCount(), 0)
})

test('the recorded timeout history stays bounded', async () => {
  for (let i = 0; i < 25; i++) {
    await assert.rejects(withRelayTimeout(new Promise(() => {}), `m${i}`, 1), /relay timeout/)
  }
  const history = getRelayTimeoutHistory()
  assert.equal(history.length, 20)
  assert.equal(history[history.length - 1].label, 'm24')
})
