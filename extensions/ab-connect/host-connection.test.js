import assert from 'node:assert/strict'
import test from 'node:test'
import { HostConnectionState } from './host-connection.js'

test('creating a Port never claims the native host is connected', () => {
  const state = new HostConnectionState()
  state.begin({})
  assert.deepEqual(state.snapshot(), {
    connected: false, connectionState: 'connecting', connectionError: null,
  })
})

test('missing native host preserves its concrete error and never confirms', () => {
  const state = new HostConnectionState()
  const port = {}
  state.begin(port)
  state.end(port, 'Specified native messaging host not found.')
  assert.deepEqual(state.snapshot(), {
    connected: false, connectionState: 'disconnected',
    connectionError: 'Specified native messaging host not found.',
  })
})

test('a pong confirms the current host once', () => {
  const state = new HostConnectionState()
  const port = {}
  state.begin(port)
  assert.equal(state.receive(port, { method: 'pong' }), true)
  assert.equal(state.receive(port, { method: 'pong' }), false)
  assert.equal(state.snapshot().connected, true)
  state.end(port, 'Native host exited')
  assert.equal(state.snapshot().connected, false)
})

test('late replies and disconnects cannot mutate a replacement connection', () => {
  const state = new HostConnectionState()
  const old = {}, current = {}
  state.begin(old)
  state.begin(current)
  assert.equal(state.receive(old, { method: 'pong' }), false)
  assert.equal(state.end(old, 'old error'), false)
  assert.equal(state.snapshot().connectionState, 'connecting')
  state.receive(current, { method: 'pong' })
  assert.equal(state.end(old, 'late old error'), false)
  assert.equal(state.snapshot().connected, true)
})

test('legacy host commands confirm the connection, malformed messages do not', () => {
  const state = new HostConnectionState(), port = {}
  state.begin(port)
  for (const msg of [null, {}, { method: 'unrecognized' }, { method: 'forwardCDPCommand' }]) {
    assert.equal(state.receive(port, msg), false)
  }
  assert.equal(state.snapshot().connected, false)
  assert.equal(state.receive(port, { method: 'forwardCDPCommand', params: { method: 'Page.getFrameTree' } }), true)
})
