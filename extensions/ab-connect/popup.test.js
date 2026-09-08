import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'

function popup(initial, extensionAvailable = true, deferred = false) {
  const callbacks = [], timers = []
  const nodes = new Map()
  let changed
  const context = {
    document: {
      getElementById(id) {
        if (!nodes.has(id)) nodes.set(id, { textContent: '', style: {},
          classList: { add() {}, remove() {} } })
        return nodes.get(id)
      },
      querySelectorAll() { return [] },
    },
    chrome: { runtime: {
      sendMessage(_request, callback) { if (deferred) callbacks.push(callback); else callback(initial) },
      onMessage: { addListener(fn) { changed = fn } },
    } },
    setTimeout(callback, delay) { timers.push({ callback, delay }) },
  }
  if (!extensionAvailable) delete context.chrome
  vm.runInNewContext(readFileSync(new URL('./popup.js', import.meta.url), 'utf8'), context)
  return { nodes, change(state) { changed({ type: 'ab-host-state', ...state }) },
    reply(index, state) { callbacks[index](state) }, pollAgain() { timers.find(t => t.delay === 700).callback() } }
}

test('popup waits for confirmation and receives a late confirmed connection', () => {
  const p = popup({ connected: false, connectionState: 'connecting' })
  assert.equal(p.nodes.get('statusLabel').textContent, 'Connecting…')
  p.change({ connected: true, connectionState: 'connected', tabCount: 2 })
  assert.equal(p.nodes.get('statusLabel').textContent, 'Connected')
  assert.equal(p.nodes.get('tabPill').textContent, '2 tabs')
})

test('native host failure updates an open popup with the actual error', () => {
  const p = popup({ connected: false, connectionState: 'connecting' })
  p.change({ connected: false, connectionState: 'disconnected',
    connectionError: 'Specified native messaging host not found.' })
  assert.equal(p.nodes.get('statusLabel').textContent, 'Not paired')
  assert.equal(p.nodes.get('statusSub').textContent, 'Specified native messaging host not found.')
})


test('standalone preview does not invent a connection or tab count', () => {
  const p = popup(null, false)
  assert.equal(p.nodes.get('statusLabel').textContent, 'Not paired')
  assert.equal(p.nodes.get('statusSub').textContent, 'Open this popup from the chrome-use extension')
  assert.equal(p.nodes.get('hint').style.display, 'none')
  assert.equal(p.nodes.get('hintCommand').textContent, '')
})


test('a disconnected installed host is diagnosed without suggesting reinstallation', () => {
  const p = popup({ connected: true, connectionState: 'connected' })
  p.change({ connected: false, connectionState: 'disconnected', connectionError: 'Native host has exited.' })
  assert.equal(p.nodes.get('hintCommand').textContent, 'chrome-use status')
  p.change({ connected: false, connectionState: 'disconnected', connectionError: 'Specified native messaging host not found.' })
  assert.equal(p.nodes.get('hintCommand').textContent, 'chrome-use extension install')
})


test('a late startup response cannot overwrite a newer pushed connection state', () => {
  const p = popup(null, true, true)
  p.change({ connected: true, connectionState: 'connected' })
  p.reply(0, { connected: false, connectionState: 'connecting' })
  assert.equal(p.nodes.get('statusLabel').textContent, 'Connected')
})

test('an older query response cannot overwrite the latest query response', () => {
  const p = popup(null, true, true)
  p.pollAgain()
  p.reply(1, { connected: true, connectionState: 'connected' })
  p.reply(0, { connected: false, connectionState: 'disconnected' })
  assert.equal(p.nodes.get('statusLabel').textContent, 'Connected')
})
