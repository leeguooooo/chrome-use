import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'

function popup(initial, extensionAvailable = true) {
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
      sendMessage(_request, callback) { callback(initial) },
      onMessage: { addListener(fn) { changed = fn } },
    } },
    setTimeout() {},
  }
  if (!extensionAvailable) delete context.chrome
  vm.runInNewContext(readFileSync(new URL('./popup.js', import.meta.url), 'utf8'), context)
  return { nodes, change(state) { changed({ type: 'ab-host-state', ...state }) } }
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
})


test('a disconnected installed host is diagnosed without suggesting reinstallation', () => {
  const p = popup({ connected: true, connectionState: 'connected' })
  p.change({ connected: false, connectionState: 'disconnected', connectionError: 'Native host has exited.' })
  assert.equal(p.nodes.get('hintCommand').textContent, 'chrome-use status')
  p.change({ connected: false, connectionState: 'disconnected', connectionError: 'Specified native messaging host not found.' })
  assert.equal(p.nodes.get('hintCommand').textContent, 'chrome-use extension install')
})
