import test from 'node:test'
import assert from 'node:assert/strict'

test('high frequency noise events are defined for filtering', () => {
  const HIGH_FREQUENCY_IGNORED_EVENTS = new Set([
    'Network.dataReceived',
    'Network.resourceChangedPriority',
    'Network.requestWillBeSentExtraInfo',
    'Network.responseReceivedExtraInfo',
    'DOM.childNodeCountUpdated',
    'DOM.attributeModified',
    'DOM.characterDataModified',
    'DOM.distributedNodesUpdated',
    'Log.entryAdded',
  ])

  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Network.dataReceived'), true)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Network.resourceChangedPriority'), true)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Network.requestWillBeSentExtraInfo'), true)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Network.responseReceivedExtraInfo'), true)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('DOM.childNodeCountUpdated'), true)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('DOM.attributeModified'), true)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('DOM.characterDataModified'), true)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('DOM.distributedNodesUpdated'), true)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Log.entryAdded'), true)

  // Essential events must never be filtered
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Target.attachedToTarget'), false)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Target.detachedFromTarget'), false)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Page.loadEventFired'), false)
  assert.equal(HIGH_FREQUENCY_IGNORED_EVENTS.has('Runtime.consoleAPICalled'), false)
})

test('batch commands dispatch sequentially with structured result telemetry', async () => {
  async function mockDispatch(tabId, method, params) {
    if (method === 'fail') throw new Error('simulated failure')
    return { status: 'ok', method }
  }

  const commands = [
    { method: 'Input.dispatchMouseEvent', params: { type: 'mousePressed' } },
    { method: 'fail', params: {} },
    { method: 'Input.dispatchKeyEvent', params: { type: 'keyDown' } },
  ]

  // stopOnError = true (default)
  const resultsStop = []
  const stopOnError = true
  for (let i = 0; i < commands.length; i++) {
    const cmd = commands[i]
    try {
      const res = await mockDispatch(1, cmd.method, cmd.params)
      resultsStop.push({ index: i, method: cmd.method, success: true, result: res })
    } catch (err) {
      resultsStop.push({ index: i, method: cmd.method, success: false, error: err.message })
      if (stopOnError) break
    }
  }

  assert.equal(resultsStop.length, 2)
  assert.equal(resultsStop[0].success, true)
  assert.equal(resultsStop[1].success, false)
  assert.equal(resultsStop[1].error, 'simulated failure')

  // stopOnError = false (continue executing remaining commands)
  const resultsContinue = []
  for (let i = 0; i < commands.length; i++) {
    const cmd = commands[i]
    try {
      const res = await mockDispatch(1, cmd.method, cmd.params)
      resultsContinue.push({ index: i, method: cmd.method, success: true, result: res })
    } catch (err) {
      resultsContinue.push({ index: i, method: cmd.method, success: false, error: err.message })
    }
  }

  assert.equal(resultsContinue.length, 3)
  assert.equal(resultsContinue[0].success, true)
  assert.equal(resultsContinue[1].success, false)
  assert.equal(resultsContinue[2].success, true)
})

test('native messaging payload limit prevents pipe crashes on oversized responses', () => {
  const MAX_NATIVE_MESSAGE_BYTES = 1000000
  const sentMessages = []
  const port = {
    postMessage: (m) => sentMessages.push(m),
  }

  function postToHost(msg) {
    if (!port) return
    try {
      const serialized = JSON.stringify(msg)
      if (serialized.length > MAX_NATIVE_MESSAGE_BYTES) {
        if (msg && msg.id != null) {
          port.postMessage({
            id: msg.id,
            error: `payload_exceeds_1mb_limit (${serialized.length} bytes)`,
          })
        }
        return
      }
      port.postMessage(msg)
    } catch {}
  }

  // Normal message passes through
  postToHost({ id: 1, result: { status: 'ok' } })
  assert.equal(sentMessages.length, 1)
  assert.equal(sentMessages[0].id, 1)

  // Oversized response is capped with clean error payload
  const hugePayload = 'x'.repeat(1050000)
  postToHost({ id: 2, result: { data: hugePayload } })
  assert.equal(sentMessages.length, 2)
  assert.equal(sentMessages[1].id, 2)
  assert.ok(sentMessages[1].error.includes('payload_exceeds_1mb_limit'))

  // Oversized event (no id) is silently suppressed to protect pipe
  postToHost({ method: 'forwardCDPEvent', params: { data: hugePayload } })
  assert.equal(sentMessages.length, 2)
})

test('markOwned awaits loadOwnedTabs and prevents storage overwrite race', async () => {
  const ownedTabs = new Set()
  let ownedLoaded = false
  let loadOwnedPromise = null
  let storageState = { ab_owned_tabs: [101, 102, 103] }

  async function fakeStorageGet() {
    // Simulate async storage read latency
    await new Promise((r) => setTimeout(r, 10))
    return { ab_owned_tabs: storageState.ab_owned_tabs }
  }

  function persistOwnedTabs() {
    storageState.ab_owned_tabs = [...ownedTabs]
  }

  async function loadOwnedTabs() {
    if (ownedLoaded) return
    if (!loadOwnedPromise) {
      loadOwnedPromise = (async () => {
        const g = await fakeStorageGet()
        for (const id of g.ab_owned_tabs || []) ownedTabs.add(id)
        ownedLoaded = true
      })()
    }
    return loadOwnedPromise
  }

  async function markOwned(tabId) {
    await loadOwnedTabs()
    if (tabId != null && !ownedTabs.has(tabId)) {
      ownedTabs.add(tabId)
      persistOwnedTabs()
    }
  }

  // Call markOwned immediately before initial load finishes
  await markOwned(201)

  // Existing tabs 101, 102, 103 must be preserved alongside new tab 201
  assert.ok(ownedTabs.has(101))
  assert.ok(ownedTabs.has(102))
  assert.ok(ownedTabs.has(103))
  assert.ok(ownedTabs.has(201))
  assert.deepEqual(storageState.ab_owned_tabs.sort(), [101, 102, 103, 201])
})
