import assert from 'node:assert/strict'
import test from 'node:test'

import { INPUT_COMMAND_TIMEOUT_MS, PAYLOAD_MAX_TIMEOUT_MS, RELAY_COMMAND_TIMEOUT_MS, getRelayTimeoutHistory, isRelayTimeoutError, relayCommandBudgetMs, relayInFlightCount, withRelayTimeout } from './relay-timeout.js'
import { sendTabCommand } from './tab-command.js'

for (const method of [
  'Input.dispatchMouseEvent',
  'Input.dispatchKeyEvent',
  'Input.dispatchTouchEvent',
  'Input.dispatchDragEvent',
  'Input.emulateTouchFromMouseEvent',
  'Input.synthesizeTapGesture',
  'Input.synthesizeScrollGesture',
  'Input.synthesizePinchGesture',
]) {
  test(`${method} gets a flat 25s input budget`, () => {
    assert.equal(relayCommandBudgetMs(method), 25000)
    assert.equal(relayCommandBudgetMs(method, { text: 'a'.repeat(50000) }), 25000)
  })
}

test('the input budget leaves time for the relay error before the CLI timeout', () => {
  assert.equal(INPUT_COMMAND_TIMEOUT_MS, 25000)
  // cli/src/native/cdp/client.rs: command_timeout uses 30s for input dispatch.
  assert.ok(INPUT_COMMAND_TIMEOUT_MS < 30000)
})

test('ordinary commands keep the flat 8s budget', () => {
  for (const method of ['Runtime.evaluate', 'Page.enable', 'Input.setIgnoreInputEvents']) {
    assert.equal(relayCommandBudgetMs(method, {}), 8000)
  }
})

test('an input timeout is not payload-scaled or replayed', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  let dispatches = 0
  let recoveries = 0
  const operation = sendTabCommand(7, 'Input.dispatchMouseEvent', { type: 'mousePressed' }, undefined, {
    sendCommand() { dispatches++; return new Promise(() => {}) },
    detachTab() { recoveries++ },
    recoverSessionTab() { recoveries++; return 7 },
  })
  const rejected = assert.rejects(operation, (error) => {
    assert.equal(isRelayTimeoutError(error), true)
    assert.match(error.message, /relay timeout after 25000ms/)
    assert.doesNotMatch(error.message, /scaled with the payload|insert was NOT cancelled/)
    return true
  })
  // Let any awaited pre-dispatch step (e.g. tab recovery) register the timer first.
  await new Promise((resolve) => setImmediate(resolve))
  t.mock.timers.tick(25000)
  await rejected
  assert.equal(dispatches, 1)
  assert.equal(recoveries, 0)
})

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

// The cap exists to stop a PATHOLOGICAL payload pinning the session, so it must
// not cut into the per-byte allowance an ordinary large insert depends on. At
// 120s it did: it bound from 56KB up, and by 150KB the EFFECTIVE rate was
// 0.8ms/byte — under the ~1.0ms/byte worst case measured on chatgpt.com. A
// healthy renderer was therefore declared failed, and since losing a
// `Promise.race` cancels nothing, the page kept working on the insert for
// minutes while the next command collided with it (#315). #309 saw the same
// thing from outside: a practical ceiling near 100KB, not the ~265KB implied.
test('the cap does not cut into the per-byte allowance at ordinary sizes', () => {
  const rate = (bytes) => relayCommandBudgetMs('Input.insertText', { text: 'a'.repeat(bytes) }) / bytes

  // The full 2ms/byte must survive well past where the old cap bound (56KB).
  for (const bytes of [56_000, 100_000, 146_000]) {
    assert.equal(
      relayCommandBudgetMs('Input.insertText', { text: 'a'.repeat(bytes) }),
      8000 + bytes * 2,
      `${bytes} bytes must still get the full per-byte allowance`,
    )
  }

  // Past the cap the effective rate decays, but must stay at or above the
  // measured worst case (1.0ms/byte) across the sizes we claim to support.
  for (const bytes of [150_000, 200_000, 300_000]) {
    assert.ok(
      rate(bytes) >= 1.0,
      `${bytes} bytes: effective ${rate(bytes)}ms/byte is under the measured worst case`,
    )
  }

  // And the ceiling still exists.
  assert.equal(relayCommandBudgetMs('Input.insertText', { text: 'a'.repeat(5_000_000) }), PAYLOAD_MAX_TIMEOUT_MS)
})

// The advice that used to be here — "Insert less at once" — is exactly what
// #301 proved corrupts text at every chunk boundary, because a call returns on
// dispatch and not on commit. The CLI-side hint was fixed; this one was not.
test('the timeout text does not advise the chunking that corrupts text', async () => {
  await assert.rejects(
    withRelayTimeout(new Promise(() => {}), 'Input.insertText', 1, { payloadScaled: true }),
    (e) => {
      assert.doesNotMatch(e.message, /Insert less at once/)
      // It must say the page is still working, or the caller sends the next
      // command straight into a busy renderer.
      assert.match(e.message, /NOT cancelled/)
      return true
    },
  )
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

test('underlying operations remain counted after wrapper expiry until they settle', async () => {
  const { relayUnresolvedOperations } = await import('./relay-timeout.js')
  const before = relayUnresolvedOperations().count
  let finish
  const underlying = new Promise(resolve => { finish = resolve })
  await assert.rejects(withRelayTimeout(underlying, 'pending operation', 5), error => {
    assert.match(error.message, /\[diag in-flight=1 oldest-in-flight=\d+ms worker-age=\d+ms\]/)
    return true
  })
  assert.equal(relayInFlightCount(), 0)
  assert.equal(relayUnresolvedOperations().count, before + 1)
  assert.ok(relayUnresolvedOperations(Date.now() + 100).oldestAgeMs >= 100)
  finish()
  await Promise.resolve()
  assert.equal(relayUnresolvedOperations().count, before)
})
