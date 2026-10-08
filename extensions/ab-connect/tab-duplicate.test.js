import assert from 'node:assert/strict'
import test from 'node:test'
import { duplicateTab } from './tab-duplicate.js'

// A fake clock for the transaction's deadlines. Nothing here reads wall-clock
// time: virtual time only moves when the test runs the clock, and then it moves
// straight to the next due timer once every pending promise has settled. A
// stage that "takes 15ms" or "never settles" therefore times out at exactly the
// same virtual instant on an idle laptop and on a CI box under load.
function fakeClock() {
  let current = 0
  let nextId = 0
  const timers = new Map()
  // setImmediate runs after the microtask queue drains, so every promise chain
  // the code under test can advance without a timer has advanced.
  const settle = () => new Promise((resolve) => setImmediate(resolve))
  const nextTimer = () => {
    let next = null
    for (const [id, timer] of timers) {
      if (!next || timer.at < next.at || (timer.at === next.at && id < next.id)) {
        next = { id, ...timer }
      }
    }
    return next
  }
  const fire = (timer) => {
    timers.delete(timer.id)
    current = timer.at
    timer.callback()
  }
  const clock = {
    now: () => current,
    setTimeout: (callback, ms = 0) => {
      const id = ++nextId
      timers.set(id, { at: current + Math.max(0, ms), callback })
      return id
    },
    clearTimeout: (id) => {
      timers.delete(id)
    },
    sleep: (ms) => new Promise((resolve) => clock.setTimeout(resolve, ms)),
    // Fire every timer due within `ms`, in order, then land exactly on it.
    async advance(ms) {
      const target = current + ms
      await settle()
      for (let timer = nextTimer(); timer && timer.at <= target; timer = nextTimer()) {
        fire(timer)
        await settle()
      }
      current = target
      await settle()
    },
    // Advance timer by timer until `predicate` holds. Fails, instead of
    // hanging, when nothing is left that could make it hold.
    async runUntil(predicate, maxTimers = 10_000) {
      await settle()
      for (let fired = 0; !predicate(); fired++) {
        const timer = nextTimer()
        if (!timer) throw new Error('fake clock: condition can never hold, no timer is pending')
        if (fired >= maxTimers) throw new Error('fake clock: too many timers fired')
        fire(timer)
        await settle()
      }
    },
    // Run the clock until `promise` settles, then hand the promise back.
    async run(promise) {
      let settled = false
      promise.then(
        () => (settled = true),
        () => (settled = true),
      )
      await clock.runUntil(() => settled)
      return promise
    },
  }
  return clock
}

const params = { sourceTargetId: 'source-target', agentGroup: 'agent-a' }
const never = () => new Promise(() => {})

// `overrides` is an object, or a function of `{ calls, clock }` for fakes that
// need to record calls or take (virtual) time.
function fixture(overrides = {}) {
  const calls = []
  const clock = fakeClock()
  const deps = {
    now: clock.now,
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
    tabForTarget: (targetId) => (targetId === 'source-target' ? 11 : null),
    getTab: async (tabId) => ({ id: tabId, windowId: 3, url: 'https://example.com/b' }),
    eligible: (tab) => Boolean(tab?.id && tab?.url),
    getLastFocusedWindow: async () => ({ id: 3 }),
    getWindow: async (windowId) => ({ id: windowId, focused: false }),
    getActiveTabs: async () => [{ id: 7 }],
    duplicateTab: async (tabId) => {
      calls.push(['duplicate', tabId])
      return { id: 22 }
    },
    markOwned: (tabId) => calls.push(['markOwned', tabId]),
    unmarkOwned: (tabId) => calls.push(['unmarkOwned', tabId]),
    groupTabInto: async (tabId, group) => calls.push(['group', tabId, group]),
    attachTab: async (tabId) => {
      calls.push(['attach', tabId])
      return { targetId: 'duplicate-target' }
    },
    completeTab: (tabId) => calls.push(['complete', tabId]),
    isolateTab: async (tabId) => calls.push(['isolate', tabId]),
    activateTab: async (tabId) => calls.push(['activate', tabId]),
    focusWindow: async () => {},
    removeTab: async (tabId) => calls.push(['remove', tabId]),
    ...(typeof overrides === 'function' ? overrides({ calls, clock }) : overrides),
  }
  // Every duplicate runs on the fake clock: a stage that would hang on real
  // timers fails loudly here instead of waiting for a test timeout.
  const run = () => clock.run(duplicateTab(params, deps))
  return { calls, clock, deps, run }
}

test('fake clock fires timers in order and only when advanced', async () => {
  const clock = fakeClock()
  const fired = []
  clock.setTimeout(() => fired.push('b'), 20)
  clock.setTimeout(() => fired.push('a'), 10)
  const cancelled = clock.setTimeout(() => fired.push('x'), 15)
  clock.clearTimeout(cancelled)
  await clock.advance(9)
  assert.deepEqual(fired, [])
  await clock.advance(1)
  assert.deepEqual(fired, ['a'])
  assert.equal(clock.now(), 10)
  await clock.advance(100)
  assert.deepEqual(fired, ['a', 'b'])
  assert.equal(clock.now(), 110)
  await assert.rejects(clock.run(never()), /no timer is pending/)
})

test('without injected time the transaction falls back to the real clock', async () => {
  const { calls, deps } = fixture()
  delete deps.now
  delete deps.setTimeout
  delete deps.clearTimeout
  const result = await duplicateTab(params, deps)
  assert.equal(result.targetId, 'duplicate-target')
  assert.ok(calls.some(([c]) => c === 'complete'))
})

test('native duplicate is grouped, attached, owned, and restores the foreground tab', async () => {
  const { calls, clock, run } = fixture()
  const result = await run()

  assert.equal(result.sourceTargetId, 'source-target')
  assert.equal(result.targetId, 'duplicate-target')
  assert.deepEqual(calls, [
    ['duplicate', 11],
    ['markOwned', 22],
    ['group', 22, 'agent-a'],
    ['attach', 22],
    ['activate', 7],
    ['complete', 22],
  ])
  assert.equal(clock.now(), 0, 'a healthy duplicate waits on no timer')
})

test('native duplicate restores the active tab and focus from another window', async () => {
  const { calls, run } = fixture(({ calls }) => ({
    getLastFocusedWindow: async () => ({ id: 9, focused: true }),
    getActiveTabs: async (windowId) => {
      calls.push(['getActiveTabs', windowId])
      return [{ id: 70 }]
    },
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  }))
  await run()

  assert.ok(calls.some((call) => call[0] === 'getActiveTabs' && call[1] === 9))
  assert.ok(calls.some((call) => call[0] === 'activate' && call[1] === 70))
  assert.ok(calls.some((call) => call[0] === 'focusWindow' && call[1] === 9))
})

test('native duplicate does not stall when foreground activation completed without resolving', async () => {
  const { clock, run } = fixture({
    transactionTimeoutMs: 100,
    cleanupTimeoutMs: 25,
    activateTab: never,
    getTab: async (tabId) => ({
      id: tabId,
      windowId: 3,
      url: 'https://example.com/b',
      active: tabId === 7,
    }),
  })

  const result = await run()

  assert.equal(result.targetId, 'duplicate-target')
  // It gave up on the hung activation after its cleanup budget, not the whole
  // transaction budget.
  assert.equal(clock.now(), 25)
})

test('native duplicate does not stall when window focus completed without resolving', async () => {
  let getWindowCalls = 0
  const { calls, clock, run } = fixture(({ calls }) => ({
    transactionTimeoutMs: 100,
    cleanupTimeoutMs: 25,
    getLastFocusedWindow: async () => ({ id: 3, focused: true }),
    focusWindow: (windowId) => {
      calls.push(['focusWindow', windowId])
      return never()
    },
    getWindow: async (windowId) => {
      getWindowCalls += 1
      return { id: windowId, focused: getWindowCalls > 1 }
    },
  }))

  const result = await run()

  assert.equal(result.targetId, 'duplicate-target')
  assert.ok(calls.some((call) => call[0] === 'focusWindow'))
  assert.equal(clock.now(), 25)
})

test('native duplicate never focuses Chrome when the user was in another app', async () => {
  // No Chrome window had focus: the user is working in another app. Restoring
  // "focus" would raise Chrome over it.
  const { calls, run } = fixture(({ calls }) => ({
    getLastFocusedWindow: async () => ({ id: 9, focused: false }),
    getWindow: async (windowId) => ({ id: windowId, focused: false }),
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  }))

  await run()

  assert.ok(calls.some((call) => call[0] === 'activate'))
  assert.ok(!calls.some((call) => call[0] === 'focusWindow'))
})

test('native duplicate skips a redundant window focus request', async () => {
  const { calls, run } = fixture(({ calls }) => ({
    getWindow: async (windowId) => ({ id: windowId, focused: true }),
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  }))

  await run()

  assert.ok(!calls.some((call) => call[0] === 'focusWindow'))
})

test('foreground restore timeout rolls back the duplicate without stalling', async () => {
  const { calls, clock, run } = fixture({
    transactionTimeoutMs: 100,
    cleanupTimeoutMs: 25,
    activateTab: never,
    getTab: async (tabId) => ({
      id: tabId,
      windowId: 3,
      url: 'https://example.com/b',
      active: false,
    }),
  })

  await assert.rejects(run(), /foreground tab restore timed out/)
  assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
  // Bounded by the transaction budget plus one cleanup budget.
  assert.ok(clock.now() <= 125, `rollback finished at ${clock.now()}ms`)
})

test('foreground restore observation timeout does not stall rollback', async () => {
  let getTabCalls = 0
  const { calls, clock, run } = fixture({
    transactionTimeoutMs: 100,
    cleanupTimeoutMs: 25,
    activateTab: never,
    getTab: async (tabId) => {
      getTabCalls += 1
      if (getTabCalls > 1) return await never()
      return { id: tabId, windowId: 3, url: 'https://example.com/b' }
    },
  })

  await assert.rejects(run(), /foreground tab restore timed out/)
  assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
  // Bounded by the transaction budget plus one cleanup budget.
  assert.ok(clock.now() <= 125, `rollback finished at ${clock.now()}ms`)
})

for (const [stage, failure, expected] of [
  ['group', { groupTabInto: never }, /tab grouping timed out/],
  ['debugger attach', { attachTab: never }, /debugger attach timed out/],
]) {
  test(`${stage} timeout rolls back the duplicate without stalling`, async () => {
    const { calls, clock, run } = fixture({
      transactionTimeoutMs: 5,
      cleanupTimeoutMs: 5,
      ...failure,
    })

    await assert.rejects(run(), expected)
    assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
    assert.equal(clock.now(), 5, 'the hung stage timed out exactly at the deadline')
  })
}

test('late native duplicate is removed after the transaction times out', async () => {
  const { calls, clock, run } = fixture(({ calls, clock }) => ({
    transactionTimeoutMs: 5,
    duplicateTab: async (tabId) => {
      await clock.sleep(15)
      calls.push(['duplicate', tabId])
      return { id: 22 }
    },
  }))

  await assert.rejects(run(), /native duplicate timed out/)
  assert.ok(!calls.some((call) => call[0] === 'remove'), 'nothing to remove yet')
  await clock.runUntil(() => calls.some((call) => call[0] === 'remove' && call[1] === 22))
  assert.equal(clock.now(), 15, 'removed as soon as the late duplicate appeared')
})

test('late native duplicate cleanup does not overwrite a newer foreground choice', async () => {
  const { calls, clock, run } = fixture(({ clock }) => ({
    transactionTimeoutMs: 5,
    getWindow: async (windowId) => ({ id: windowId, focused: true }),
    duplicateTab: async () => {
      await clock.sleep(15)
      return { id: 22 }
    },
  }))

  await assert.rejects(run(), /native duplicate timed out/)
  const restoreCallsAfterTimeout = calls.filter((call) => call[0] === 'activate').length
  await clock.runUntil(() => calls.some((call) => call[0] === 'remove' && call[1] === 22))

  assert.equal(
    calls.filter((call) => call[0] === 'activate').length,
    restoreCallsAfterTimeout,
  )
})

test('duplicate setup stages share one transaction deadline', async () => {
  const { calls, clock, run } = fixture(({ clock }) => ({
    transactionTimeoutMs: 30,
    duplicateTab: async () => {
      await clock.sleep(20)
      return { id: 22 }
    },
    // 20ms fits a fresh 30ms budget, but not the 10ms the duplicate left.
    groupTabInto: async () => {
      await clock.sleep(20)
    },
  }))

  await assert.rejects(run(), /tab grouping timed out/)
  assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
  assert.ok(clock.now() < 40, `grouping got its own budget (failed at ${clock.now()}ms)`)
})

test('late debugger attach observes that its duplicate transaction was cancelled', async () => {
  const { calls, clock, run } = fixture(({ calls, clock }) => ({
    transactionTimeoutMs: 5,
    attachTab: async (tabId, transactionIsActive) => {
      await clock.sleep(15)
      calls.push(['attachActive', tabId, transactionIsActive?.()])
      if (!transactionIsActive?.()) throw new Error('attach cancelled')
      return { targetId: 'duplicate-target' }
    },
  }))

  await assert.rejects(run(), /debugger attach timed out/)
  await clock.runUntil(() => calls.some((call) => call[0] === 'attachActive'))
  assert.ok(calls.some((call) => call[0] === 'attachActive' && call[2] === false))
})

test('native duplicate failure is returned without orphan cleanup', async () => {
  const { calls, run } = fixture({
    duplicateTab: async () => Promise.reject(new Error('native duplicate failed')),
  })
  await assert.rejects(run(), /native duplicate failed/)
  assert.ok(!calls.some((call) => call[0] === 'unmarkOwned'))
  assert.ok(!calls.some((call) => call[0] === 'remove'))
})

test('rollback continues after unmark ownership throws', async () => {
  const original = new Error('group is unavailable')
  const { calls, run } = fixture(({ calls }) => ({
    groupTabInto: async () => Promise.reject(original),
    unmarkOwned: () => {
      calls.push(['unmarkOwned'])
      throw new Error('unmark failed')
    },
    getLastFocusedWindow: async () => ({ id: 3, focused: true }),
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  }))

  await assert.rejects(run(), (error) => error === original)
  assert.ok(calls.some((call) => call[0] === 'remove'))
  assert.ok(calls.some((call) => call[0] === 'activate'))
  assert.ok(calls.some((call) => call[0] === 'focusWindow'))
})

test('rollback continues after tab removal rejects', async () => {
  const original = new Error('attach is unavailable')
  const { calls, run } = fixture(({ calls }) => ({
    attachTab: async () => Promise.reject(original),
    removeTab: async (tabId) => {
      calls.push(['remove', tabId])
      throw new Error('remove failed')
    },
    getLastFocusedWindow: async () => ({ id: 3, focused: true }),
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  }))

  await assert.rejects(run(), (error) => error === original)
  assert.ok(calls.some((call) => call[0] === 'activate'))
  assert.ok(calls.some((call) => call[0] === 'focusWindow'))
})

test('rollback cleanup stages share one aggregate deadline', async () => {
  const { calls, clock, run } = fixture(({ calls }) => ({
    cleanupTimeoutMs: 250,
    groupTabInto: async () => {
      throw new Error('group failed')
    },
    unmarkOwned: (tabId) => {
      calls.push(['unmarkOwned', tabId])
      return never()
    },
    isolateTab: (tabId) => {
      calls.push(['isolate', tabId])
      return never()
    },
    removeTab: (tabId) => {
      calls.push(['remove', tabId])
      return never()
    },
    activateTab: (tabId) => {
      calls.push(['activate', tabId])
      return never()
    },
    getWindow: never,
    getLastFocusedWindow: async () => ({ id: 3, focused: true }),
    focusWindow: (windowId) => {
      calls.push(['focusWindow', windowId])
      return never()
    },
  }))

  await assert.rejects(run(), /group failed/)

  for (const operation of ['unmarkOwned', 'isolate', 'remove', 'activate', 'focusWindow']) {
    assert.ok(calls.some((call) => call[0] === operation), `${operation} was not attempted`)
  }
  // Five stalled stages, one 250ms budget between them — not 250ms each.
  assert.equal(clock.now(), 250)
})

test('failed rollback does not commit the duplicate transaction', async () => {
  const { calls, run } = fixture({
    activateTab: async () => {
      throw new Error('restore failed')
    },
    getTab: async (tabId) => ({
      id: tabId,
      windowId: 3,
      url: 'https://example.com/b',
      active: false,
    }),
    removeTab: async () => {
      throw new Error('remove failed')
    },
  })

  await assert.rejects(run(), /restore failed/)
  assert.ok(!calls.some((call) => call[0] === 'complete'))
})

test('rollback isolates the duplicate before a stalled removal', async () => {
  const { calls, clock, run } = fixture({
    cleanupTimeoutMs: 5,
    groupTabInto: async () => {
      throw new Error('group failed')
    },
    removeTab: never,
  })

  await assert.rejects(run(), /group failed/)
  assert.ok(calls.some((call) => call[0] === 'isolate' && call[1] === 22))
  assert.equal(clock.now(), 5, 'the stalled removal was abandoned at the cleanup deadline')
})

test('rollback focuses the previous window after tab activation rejects', async () => {
  const original = new Error('setup failed')
  const { calls, run } = fixture(({ calls }) => ({
    attachTab: async () => Promise.reject(original),
    activateTab: async (tabId) => {
      calls.push(['activate', tabId])
      throw new Error('activate failed')
    },
    getLastFocusedWindow: async () => ({ id: 3, focused: true }),
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  }))

  await assert.rejects(run(), (error) => error === original)
  assert.ok(calls.some((call) => call[0] === 'focusWindow'))
})

for (const [stage, failure] of [
  ['group', { groupTabInto: async () => Promise.reject(new Error('group failed')) }],
  ['attach', { attachTab: async () => Promise.reject(new Error('attach failed')) }],
  ['restore', { activateTab: async () => Promise.reject(new Error('restore failed')) }],
]) {
  test(`${stage} failure removes the orphaned duplicate`, async () => {
    const { calls, run } = fixture(failure)
    await assert.rejects(run(), new RegExp(`${stage} failed`))
    assert.ok(calls.some((call) => call[0] === 'unmarkOwned' && call[1] === 22))
    assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
  })
}

// The three inspection reads are bounded by the transaction deadline, but a read
// that fails must still degrade the way it did before #342 rather than reject
// the whole duplicate.
test('no focused window falls back to the source window instead of failing', async () => {
  const { calls, run } = fixture({
    getLastFocusedWindow: async () => {
      throw new Error('no window has focus')
    },
  })
  const result = await run()
  assert.equal(result.targetId, 'duplicate-target')
  assert.ok(calls.some(([c]) => c === 'attach'), 'the duplicate went through')
})

test('no active tab restores the source tab instead of failing', async () => {
  const { calls, run } = fixture({
    getActiveTabs: async () => {
      throw new Error('tabs.query failed')
    },
  })
  const result = await run()
  assert.equal(result.targetId, 'duplicate-target')
  assert.deepEqual(calls.find(([c]) => c === 'activate'), ['activate', 11])
})

test('a source tab that cannot be read is reported as not eligible', async () => {
  const { calls, run } = fixture({
    getTab: async () => {
      throw new Error('No tab with id: 11')
    },
  })
  await assert.rejects(run(), /not eligible/)
  assert.deepEqual(calls, [], 'nothing was duplicated')
})

// Once the transaction deadline has passed, no later stage may start. A 0ms
// `withTimeout` does not enforce that — an operation settling in a microtask
// beats the 0ms timer — so without the explicit guard every side-effecting stage
// (own, group, attach, activate) ran after the deadline and the duplicate
// "succeeded" late.
test('stages do not run once the transaction deadline has passed', async () => {
  const { calls, clock, run } = fixture(({ clock }) => ({
    transactionTimeoutMs: 20,
    getActiveTabs: async () => {
      await clock.sleep(40)
      return [{ id: 7 }]
    },
  }))
  await assert.rejects(run(), /timed out/)
  // None of the stages that build the duplicate ran...
  for (const stage of ['markOwned', 'group', 'attach', 'complete']) {
    assert.ok(!calls.some(([c]) => c === stage), `${stage} must not run after the deadline`)
  }
  // ...and the rollback still did its job: the foreground went back (to the
  // source tab, since reading the active tab timed out) and the duplicate that
  // Chrome had already made was removed.
  await clock.runUntil(() => calls.some(([c]) => c === 'remove'))
  assert.deepEqual(calls.find(([c]) => c === 'activate'), ['activate', 11])
  assert.deepEqual(calls.find(([c]) => c === 'remove'), ['remove', 22])
})
