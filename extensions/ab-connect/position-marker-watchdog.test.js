import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'

import { createMarkerWatchdog, MARKER_WATCHDOG_MS } from './position-marker-watchdog.js'

function fakeTimers() {
  let next = 1
  const timers = new Map()
  return {
    schedule: (fn, ms) => { const id = next++; timers.set(id, { fn, ms }); return id },
    cancel: id => timers.delete(id),
    fire() { const all = [...timers.values()]; timers.clear(); for (const t of all) t.fn() },
    timers,
  }
}

const draw = { hingeConfig: { rect: { x: 1, y: 2, width: 16, height: 16 } } }

test('a draw the daemon never clears is cleared by the worker (#850)', async () => {
  const t = fakeTimers(); const cleared = []
  const w = createMarkerWatchdog({ clear: id => cleared.push(id), ...t })
  w.observe(7, 'Overlay.setShowHinge', draw)
  assert.deepEqual([...t.timers.values()].map(x => x.ms), [MARKER_WATCHDOG_MS])
  t.fire(); await Promise.resolve(); await Promise.resolve()
  assert.deepEqual(cleared, [7])
  assert.deepEqual(w.pending(), [])
})

test("the daemon's own clear, a newer draw, or the tab going away cancel it", () => {
  const t = fakeTimers(); const cleared = []
  const w = createMarkerWatchdog({ clear: id => cleared.push(id), ...t })
  w.observe(1, 'Overlay.setShowHinge', draw)
  w.observe(1, 'Overlay.setShowHinge', {})
  assert.equal(t.timers.size, 0)
  w.observe(2, 'Overlay.setShowHinge', draw)
  w.observe(2, 'Overlay.setShowHinge', draw)
  assert.equal(t.timers.size, 1)
  w.forget(2)
  assert.equal(t.timers.size, 0)
  t.fire()
  assert.deepEqual(cleared, [])
})

test('other methods, iframe sessions and unknown tabs are ignored', () => {
  const t = fakeTimers()
  const w = createMarkerWatchdog({ clear: () => {}, ...t })
  w.observe(1, 'Input.dispatchMouseEvent', draw)
  w.observe(1, 'Overlay.setShowHinge', draw, 'child-session')
  w.observe(null, 'Overlay.setShowHinge', draw)
  assert.equal(t.timers.size, 0)
})

test('the worker wires it next to the relayed command and on tab removal', () => {
  const bg = readFileSync(new URL('./background.js', import.meta.url), 'utf8')
  assert.match(bg, /markerWatchdog\.observe\(tabId, method, params, childSid\)/)
  assert.ok((bg.match(/markerWatchdog\.forget\(/g) || []).length >= 3)
  // Clearing never attaches: it uses the existing attachment only.
  const clear = bg.slice(bg.indexOf('const markerWatchdog'), bg.indexOf('const markerWatchdog') + 600)
  assert.match(clear, /chrome\.debugger\.sendCommand\(\{ tabId \}, 'Overlay\.setShowHinge', \{\}\)/)
  assert.doesNotMatch(clear, /sendCdpToTab|attachTab|chrome\.debugger\.attach/)
})
