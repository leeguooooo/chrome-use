import assert from 'node:assert/strict'
import test from 'node:test'

import { isRelayTimeoutError, withRelayTimeout } from './relay-timeout.js'

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
