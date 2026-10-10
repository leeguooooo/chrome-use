// #530: a reply over Chrome's 64 MiB native-messaging limit used to vanish
// (postMessage threw, the worker took it for a dead port), so the CLI timed
// out. Pure helpers first, then the real postToHost and onHostMessage in a vm
// with a port that refuses oversized messages the way Chrome does.
import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'

import {
  NATIVE_MESSAGE_LIMIT_BYTES,
  REPLY_TOO_LARGE,
  isMessageTooLargeError,
  oversizeReplyError,
  replyTooLargeMessage,
  utf8Length,
} from './host-message-size.js'

const CHROME_ERROR = 'Message length exceeded maximum allowed length.'

test('the limit is Chrome\'s 64 MiB', () => {
  assert.equal(NATIVE_MESSAGE_LIMIT_BYTES, 67108864)
})

test('only Chrome\'s size refusal counts as too large', () => {
  assert.equal(isMessageTooLargeError(new Error(CHROME_ERROR)), true)
  assert.equal(isMessageTooLargeError('Message too large'), true)
  assert.equal(isMessageTooLargeError(new Error('Attempting to use a disconnected port object')), false)
  assert.equal(isMessageTooLargeError(undefined), false)
})

test('utf8 length counts bytes, not UTF-16 units', () => {
  assert.equal(utf8Length('abc'), 3)
  assert.equal(utf8Length('é'), 2)
  assert.equal(utf8Length('中'), 3)
  assert.equal(utf8Length('😀'), 4)
  assert.equal(utf8Length('a中😀'), Buffer.byteLength('a中😀'))
})

test('the error names the command, the size and the limit, and is not retryable', () => {
  const m = replyTooLargeMessage('Runtime.evaluate', 70 * 1024 * 1024)
  assert.ok(m.startsWith(`${REPLY_TOO_LARGE}: the reply to Runtime.evaluate is 70.0 MiB, over Chrome's 64.0 MiB limit`), m)
  assert.match(m, /Nothing is retried/)
  assert.ok(replyTooLargeMessage(undefined, null).startsWith(`${REPLY_TOO_LARGE}: the reply is over`))
})

test('only a reply whose post failed on size gets an error reply', () => {
  const msg = { id: 9, result: { value: 'x'.repeat(10) } }
  const r = oversizeReplyError(msg, new Error(CHROME_ERROR), 'Runtime.evaluate')
  assert.equal(r.id, 9)
  assert.equal(r.result, undefined)
  assert.ok(r.error.includes('reply_too_large'), r.error)
  assert.ok(r.error.includes(`is 0.0 MiB`), r.error)
  assert.equal(oversizeReplyError(msg, new Error('disconnected port'), 'X'), null, 'a dead port reconnects')
  assert.equal(oversizeReplyError({ method: 'forwardCDPEvent' }, new Error(CHROME_ERROR)), null, 'an event has no caller')
})

// ---- the real handlers ------------------------------------------------------

const SOURCE = readFileSync(new URL('./background.js', import.meta.url), 'utf8')
function slice(start, end) {
  const from = SOURCE.indexOf(start)
  assert.ok(from >= 0, `background.js has ${start}`)
  const to = SOURCE.indexOf(end, from)
  assert.ok(to > from, `background.js has ${end} after ${start}`)
  return SOURCE.slice(from, to)
}

// A port that refuses, as Chrome does, any message whose JSON is over `limit`.
function worker(limit) {
  const delivered = []
  const context = {
    console,
    oversizeReplyError,
    port: {
      postMessage(msg) {
        if (JSON.stringify(msg).length > limit) throw new Error(CHROME_ERROR)
        delivered.push(msg)
      },
    },
    scheduleKeepalivePing() {},
    reannounceAttachedTabs: async () => {},
    reattachOwnedTabs: async () => {},
    syncRelayTargets() {},
    handleForwardCdpCommand: async () => ({}),
    scheduleUpdateApply() {},
  }
  vm.createContext(context)
  vm.runInContext(slice('// `method` names the command a reply answers', 'function setBadge('), context)
  vm.runInContext('let hostCommandsInFlight = 0; let lastHostCommandAt = 0; let updatePending = false;', context)
  vm.runInContext(slice('async function onHostMessage(', '// ---- CDP command dispatch'), context)
  return { context, delivered }
}

test('an oversized reply reaches the caller at once as an error naming the limit', async () => {
  const w = worker(1000)
  w.context.handleForwardCdpCommand = async () => ({ result: { type: 'string', value: 'x'.repeat(5000) } })
  await w.context.onHostMessage({ id: 41, method: 'forwardCDPCommand', params: { method: 'Runtime.evaluate' } })
  assert.equal(w.delivered.length, 1, 'exactly one answer, not silence')
  assert.equal(w.delivered[0].id, 41)
  assert.equal('result' in w.delivered[0], false)
  assert.match(w.delivered[0].error, /^reply_too_large: the reply to Runtime\.evaluate is 0\.0 MiB, over Chrome's 64\.0 MiB limit/)
})

test('a reply under the limit is delivered unchanged', async () => {
  const w = worker(1000)
  w.context.handleForwardCdpCommand = async () => ({ ok: 1 })
  await w.context.onHostMessage({ id: 42, method: 'forwardCDPCommand', params: { method: 'Page.navigate' } })
  assert.deepEqual(w.delivered, [{ id: 42, result: { ok: 1 } }])
})

test('an oversized event is dropped without breaking anything', () => {
  const w = worker(100)
  w.context.postToHost({ method: 'forwardCDPEvent', params: { big: 'x'.repeat(500) } })
  assert.deepEqual(w.delivered, [])
  w.context.postToHost({ method: 'ping' })
  assert.deepEqual(w.delivered, [{ method: 'ping' }])
})
