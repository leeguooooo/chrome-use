import assert from 'node:assert/strict'
import test from 'node:test'

import { PAYLOAD_MAX_TIMEOUT_MS, RELAY_COMMAND_TIMEOUT_MS, getRelayTimeoutHistory, isRelayTimeoutError, relayCommandBudgetMs, relayInFlightCount, withRelayTimeout } from './relay-timeout.js'

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
  try {
    await assert.rejects(
      withRelayTimeout(new Promise(() => {}), 'Runtime.evaluate', 20),
      (error) => {
        const diag = error.relayDiagnostics
        assert.equal(diag.label, 'Runtime.evaluate')
        // Date.now() and timer scheduling have different clock granularity on
        // hosted runners; a nominal 20ms timer can be observed as 19ms. The
        // contract is a finite, non-negative elapsed measurement, not exact
        // millisecond equality with the configured budget.
        assert.equal(Number.isFinite(diag.elapsedMs), true)
        assert.ok(diag.elapsedMs >= 0)
        assert.equal(diag.inFlight, 2)
        assert.ok(diag.oldestInFlightMs >= diag.elapsedMs)
        // The worker cannot be younger than the command it timed out: a pending
        // timer dies with its context, so an eviction mid-command never surfaces
        // as this error at all.
        assert.ok(diag.workerAgeMs >= diag.elapsedMs)
        assert.match(
          error.message,
          /\[diag in-flight=2 oldest-in-flight=\d+ms worker-age=\d+ms\]/,
        )
        return true
      },
    )
    assert.equal(getRelayTimeoutHistory().length, before + 1)
  } finally {
    // Always consume the parallel timeout. If an assertion above fails, leaving
    // this promise behind creates an unhandled rejection and pollutes the next
    // test's in-flight count.
    await assert.rejects(parallel, /relay timeout/)
  }
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

test('a payload-sized command gets a budget proportional to its payload', () => {
  // An ordinary command keeps the flat budget — scaling must not slow down the
  // failure of a genuinely hung round trip.
  assert.equal(relayCommandBudgetMs('Runtime.evaluate', { expression: 'x'.repeat(50000) }), RELAY_COMMAND_TIMEOUT_MS)
  assert.equal(relayCommandBudgetMs('Page.navigate', { url: 'https://example.com' }), RELAY_COMMAND_TIMEOUT_MS)
  assert.equal(relayCommandBudgetMs('Input.insertText', {}), RELAY_COMMAND_TIMEOUT_MS)
  assert.equal(relayCommandBudgetMs('Input.insertText', { text: '' }), RELAY_COMMAND_TIMEOUT_MS)

  // insertText scales: the 20KB payload that used to hit the flat 8s wall now
  // gets room. (Measured ~0.45s/KB in a rich editor; 2ms/byte is ~4x that.)
  assert.equal(relayCommandBudgetMs('Input.insertText', { text: 'a'.repeat(20000) }), 8000 + 40000)
  // 34KB — the size that motivated `keyboard inserttext --file` (#301).
  assert.equal(relayCommandBudgetMs('Input.insertText', { text: 'a'.repeat(34000) }), 8000 + 68000)
  // Capped, so a pathological payload cannot pin the session forever.
  assert.equal(relayCommandBudgetMs('Input.insertText', { text: 'a'.repeat(10_000_000) }), PAYLOAD_MAX_TIMEOUT_MS)
})

test('a scaled-budget timeout does not blame an unresponsive debugger', async () => {
  const never = new Promise(() => {})
  await assert.rejects(
    withRelayTimeout(never, 'chrome.debugger.sendCommand(Input.insertText)', 30, { payloadScaled: true }),
    (e) => {
      // It was given payload-proportional time, so "the debugger stopped
      // responding" would send the caller after a hang that isn't there.
      assert.match(e.message, /budget already scaled with the payload size/)
      assert.doesNotMatch(e.message, /stopped responding/)
      return true
    },
  )
  await assert.rejects(
    withRelayTimeout(new Promise(() => {}), 'chrome.debugger.sendCommand(DOM.getDocument)', RELAY_COMMAND_TIMEOUT_MS),
    (e) => {
      assert.match(e.message, /stopped responding/)
      return true
    },
  )
})
