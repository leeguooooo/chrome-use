import assert from 'node:assert/strict'
import test from 'node:test'

import { sendTabCommand } from './tab-command.js'

function fixture(error, recoveredTab = 42) {
  const sent = []
  const detached = []
  const recovered = []
  return {
    sent, detached, recovered,
    deps: {
      async sendCommand(target, method, params) {
        sent.push({ target, method, params })
        if (sent.length === 1) throw error
        return { frameTree: { frame: { url: 'https://example.com/after' } } }
      },
      detachTab(...args) { detached.push(args) },
      async recoverSessionTab(session) { recovered.push(session); return recoveredTab },
    },
  }
}

test('top-level read follows the recovered tab rather than retrying the dead tab', async () => {
  const f = fixture(new Error('Target closed'))
  const result = await sendTabCommand(7, 'Page.getFrameTree', undefined, undefined, f.deps)
  assert.equal(result.frameTree.frame.url, 'https://example.com/after')
  assert.deepEqual(f.sent.map(x => x.target), [{ tabId: 7 }, { tabId: 42 }])
  assert.deepEqual(f.recovered, ['cb-tab-7'])
})

test('a restricted child frame does not detach or rebind its healthy parent tab', async () => {
  const error = new Error('Cannot access a chrome-extension:// URL of different extension')
  const f = fixture(error)
  await assert.rejects(
    sendTabCommand(7, 'Accessibility.getFullAXTree', {}, 'child-frame', f.deps),
    e => e === error,
  )
  assert.equal(f.sent.length, 1)
  assert.deepEqual(f.sent[0].target, { tabId: 7, sessionId: 'child-frame' })
  assert.deepEqual(f.detached, [])
  assert.deepEqual(f.recovered, [])
})

test('a detached child session is not replayed after reattaching its parent', async () => {
  const error = new Error('Session detached')
  const f = fixture(error)
  await assert.rejects(
    sendTabCommand(7, 'Runtime.evaluate', { expression: 'document.title' }, 'child-frame', f.deps),
    e => e === error,
  )
  assert.equal(f.sent.length, 1)
  assert.deepEqual(f.detached, [])
})

test('failed top-level recovery preserves the original error without another dispatch', async () => {
  const error = new Error('Target closed')
  const f = fixture(error, null)
  await assert.rejects(sendTabCommand(7, 'Page.getFrameTree', {}, undefined, f.deps), e => e === error)
  assert.equal(f.sent.length, 1)
})

test('ordinary page errors do not trigger attachment recovery', async () => {
  const error = new Error('Invalid parameters')
  const f = fixture(error)
  await assert.rejects(sendTabCommand(7, 'DOM.resolveNode', {}, undefined, f.deps), e => e === error)
  assert.deepEqual(f.detached, [])
  assert.deepEqual(f.recovered, [])
})

test('parent observation still works after a child permission error', async () => {
  const error = new Error('Cannot access a chrome-extension:// URL of different extension')
  const f = fixture(error)
  await assert.rejects(sendTabCommand(7, 'DOM.getDocument', {}, 'child-frame', f.deps))
  await sendTabCommand(7, 'Page.getFrameTree', {}, undefined, f.deps)
  assert.deepEqual(f.sent.map(x => x.target), [
    { tabId: 7, sessionId: 'child-frame' }, { tabId: 7 },
  ])
  assert.deepEqual(f.recovered, [])
})

test('relay timeout does not replay a possibly completed action', async () => {
  const error = new Error('relay timeout: Runtime.evaluate')
  error.name = 'RelayTimeoutError'
  const f = fixture(error)
  await assert.rejects(sendTabCommand(7, 'Runtime.evaluate', {}, undefined, f.deps), e => e === error)
  assert.equal(f.sent.length, 1)
  assert.deepEqual(f.detached, [])
})

test('recovery is bounded when the replacement also fails', async () => {
  let attempts = 0
  let recoveries = 0
  const error = new Error('Target closed')
  await assert.rejects(sendTabCommand(7, 'Page.getFrameTree', {}, undefined, {
    async sendCommand() { attempts++; throw error },
    detachTab() {},
    async recoverSessionTab() { recoveries++; return 42 },
  }), e => e === error)
  assert.equal(attempts, 2)
  assert.equal(recoveries, 1)
})

test('an action that changes the page before detaching is not executed twice', async () => {
  let submissions = 0
  let recoveryCalls = 0
  await assert.rejects(sendTabCommand(7, 'Runtime.evaluate', {
    expression: 'document.querySelector("form").requestSubmit()',
  }, undefined, {
    async sendCommand() {
      submissions++
      throw new Error('Detached while handling command')
    },
    detachTab() { recoveryCalls++ },
    async recoverSessionTab() { recoveryCalls++; return 7 },
  }), /action_outcome_unknown:.*not replayed/)
  assert.equal(submissions, 1)
  assert.equal(recoveryCalls, 0)
})

test('explicit rejection before dispatch can reattach and execute an action once', async () => {
  const f = fixture(new Error('Debugger is not attached to the tab with id: 7.'), 7)
  await sendTabCommand(7, 'Input.insertText', { text: 'fixture' }, undefined, f.deps)
  assert.equal(f.sent.length, 2)
  assert.deepEqual(f.recovered, ['cb-tab-7'])
})

test('a lost reply after pre-dispatch recovery still forbids another action retry', async () => {
  let attempts = 0
  let submissions = 0
  await assert.rejects(sendTabCommand(7, 'Runtime.evaluate', {}, undefined, {
    async sendCommand() {
      attempts++
      if (attempts === 1) throw new Error('Debugger is not attached to the tab with id: 7.')
      submissions++
      throw new Error('Detached while handling command')
    },
    detachTab() {},
    async recoverSessionTab() { return 7 },
  }), /action_outcome_unknown:/)
  assert.equal(attempts, 2)
  assert.equal(submissions, 1)
})
