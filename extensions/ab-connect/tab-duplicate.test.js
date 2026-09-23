import assert from 'node:assert/strict'
import test from 'node:test'
import { duplicateTab } from './tab-duplicate.js'

function fixture(overrides = {}) {
  const calls = []
  const deps = {
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
    ...overrides,
  }
  return { calls, deps }
}

const params = { sourceTargetId: 'source-target', agentGroup: 'agent-a' }
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
async function waitUntil(predicate, timeoutMs = 250) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (predicate()) return true
    await wait(5)
  }
  return predicate()
}

test('native duplicate is grouped, attached, owned, and restores the foreground tab', async () => {
  const { calls, deps } = fixture()
  const result = await duplicateTab(params, deps)

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
})

test('native duplicate restores the active tab and focus from another window', async () => {
  const { calls, deps } = fixture({
    getLastFocusedWindow: async () => ({ id: 9 }),
    getActiveTabs: async (windowId) => {
      calls.push(['getActiveTabs', windowId])
      return [{ id: 70 }]
    },
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  })
  await duplicateTab(params, deps)

  assert.ok(calls.some((call) => call[0] === 'getActiveTabs' && call[1] === 9))
  assert.ok(calls.some((call) => call[0] === 'activate' && call[1] === 70))
  assert.ok(calls.some((call) => call[0] === 'focusWindow' && call[1] === 9))
})

test('native duplicate does not stall when foreground activation completed without resolving', { timeout: 200 }, async () => {
  const { deps } = fixture({
    transactionTimeoutMs: 20,
    cleanupTimeoutMs: 5,
    activateTab: () => new Promise(() => {}),
    getTab: async (tabId) => ({
      id: tabId,
      windowId: 3,
      url: 'https://example.com/b',
      active: tabId === 7,
    }),
  })

  const result = await duplicateTab(params, deps)

  assert.equal(result.targetId, 'duplicate-target')
})

test('native duplicate does not stall when window focus completed without resolving', { timeout: 200 }, async () => {
  let getWindowCalls = 0
  const { calls, deps } = fixture({
    transactionTimeoutMs: 20,
    cleanupTimeoutMs: 5,
    focusWindow: (windowId) => {
      calls.push(['focusWindow', windowId])
      return new Promise(() => {})
    },
    getWindow: async (windowId) => {
      getWindowCalls += 1
      return { id: windowId, focused: getWindowCalls > 1 }
    },
  })

  const result = await duplicateTab(params, deps)

  assert.equal(result.targetId, 'duplicate-target')
  assert.ok(calls.some((call) => call[0] === 'focusWindow'))
})

test('native duplicate skips a redundant window focus request', async () => {
  const { calls, deps } = fixture({
    getWindow: async (windowId) => ({ id: windowId, focused: true }),
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  })

  await duplicateTab(params, deps)

  assert.ok(!calls.some((call) => call[0] === 'focusWindow'))
})

test('foreground restore timeout rolls back the duplicate without stalling', { timeout: 200 }, async () => {
  const { calls, deps } = fixture({
    transactionTimeoutMs: 20,
    cleanupTimeoutMs: 5,
    activateTab: () => new Promise(() => {}),
    getTab: async (tabId) => ({
      id: tabId,
      windowId: 3,
      url: 'https://example.com/b',
      active: false,
    }),
  })

  await assert.rejects(duplicateTab(params, deps), /foreground tab restore timed out/)
  assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
})

test('foreground restore observation timeout does not stall rollback', { timeout: 200 }, async () => {
  let getTabCalls = 0
  const { calls, deps } = fixture({
    transactionTimeoutMs: 20,
    cleanupTimeoutMs: 5,
    activateTab: () => new Promise(() => {}),
    getTab: async (tabId) => {
      getTabCalls += 1
      if (getTabCalls > 1) return await new Promise(() => {})
      return { id: tabId, windowId: 3, url: 'https://example.com/b' }
    },
  })

  await assert.rejects(duplicateTab(params, deps), /foreground tab restore timed out/)
  assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
})

for (const [stage, failure, expected] of [
  ['group', { groupTabInto: () => new Promise(() => {}) }, /tab grouping timed out/],
  ['debugger attach', { attachTab: () => new Promise(() => {}) }, /debugger attach timed out/],
]) {
  test(`${stage} timeout rolls back the duplicate without stalling`, { timeout: 200 }, async () => {
    const { calls, deps } = fixture({ transactionTimeoutMs: 5, ...failure })

    await assert.rejects(duplicateTab(params, deps), expected)
    assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
  })
}

test('late native duplicate is removed after the transaction times out', async () => {
  const { calls, deps } = fixture({
    transactionTimeoutMs: 5,
    duplicateTab: async (tabId) => {
      await wait(15)
      calls.push(['duplicate', tabId])
      return { id: 22 }
    },
  })

  await assert.rejects(duplicateTab(params, deps), /native duplicate timed out/)
  assert.ok(
    await waitUntil(() => calls.some((call) => call[0] === 'remove' && call[1] === 22)),
  )
})

test(
  'late native duplicate cleanup does not overwrite a newer foreground choice',
  { timeout: 200 },
  async () => {
    const { calls, deps } = fixture({
      transactionTimeoutMs: 5,
      getWindow: async (windowId) => ({ id: windowId, focused: true }),
      duplicateTab: async () => {
        await wait(15)
        return { id: 22 }
      },
    })

    await assert.rejects(duplicateTab(params, deps), /native duplicate timed out/)
    const restoreCallsAfterTimeout = calls.filter((call) => call[0] === 'activate').length
    assert.ok(
      await waitUntil(() => calls.some((call) => call[0] === 'remove' && call[1] === 22)),
    )

    assert.equal(
      calls.filter((call) => call[0] === 'activate').length,
      restoreCallsAfterTimeout,
    )
  },
)

test('duplicate setup stages share one transaction deadline', { timeout: 500 }, async () => {
  const { calls, deps } = fixture({
    transactionTimeoutMs: 30,
    duplicateTab: async () => {
      await wait(20)
      return { id: 22 }
    },
    groupTabInto: async () => {
      await wait(20)
    },
  })

  await assert.rejects(duplicateTab(params, deps), /tab grouping timed out/)
  assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
})

test('late debugger attach observes that its duplicate transaction was cancelled', async () => {
  const { calls, deps } = fixture({
    transactionTimeoutMs: 5,
    attachTab: async (tabId, transactionIsActive) => {
      await wait(15)
      calls.push(['attachActive', tabId, transactionIsActive?.()])
      if (!transactionIsActive?.()) throw new Error('attach cancelled')
      return { targetId: 'duplicate-target' }
    },
  })

  await assert.rejects(duplicateTab(params, deps), /debugger attach timed out/)
  assert.ok(
    await waitUntil(() =>
      calls.some((call) => call[0] === 'attachActive' && call[2] === false),
    ),
  )
})

test('native duplicate failure is returned without orphan cleanup', async () => {
  const { calls, deps } = fixture({
    duplicateTab: async () => Promise.reject(new Error('native duplicate failed')),
  })
  await assert.rejects(duplicateTab(params, deps), /native duplicate failed/)
  assert.ok(!calls.some((call) => call[0] === 'unmarkOwned'))
  assert.ok(!calls.some((call) => call[0] === 'remove'))
})

test('rollback continues after unmark ownership throws', async () => {
  const original = new Error('group is unavailable')
  const { calls, deps } = fixture({
    groupTabInto: async () => Promise.reject(original),
    unmarkOwned: () => {
      calls.push(['unmarkOwned'])
      throw new Error('unmark failed')
    },
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  })

  await assert.rejects(duplicateTab(params, deps), (error) => error === original)
  assert.ok(calls.some((call) => call[0] === 'remove'))
  assert.ok(calls.some((call) => call[0] === 'activate'))
  assert.ok(calls.some((call) => call[0] === 'focusWindow'))
})

test('rollback continues after tab removal rejects', async () => {
  const original = new Error('attach is unavailable')
  const { calls, deps } = fixture({
    attachTab: async () => Promise.reject(original),
    removeTab: async (tabId) => {
      calls.push(['remove', tabId])
      throw new Error('remove failed')
    },
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  })

  await assert.rejects(duplicateTab(params, deps), (error) => error === original)
  assert.ok(calls.some((call) => call[0] === 'activate'))
  assert.ok(calls.some((call) => call[0] === 'focusWindow'))
})

test('rollback cleanup stages share one aggregate deadline', { timeout: 1000 }, async () => {
  const stalled = () => new Promise(() => {})
  const { calls, deps } = fixture({
    cleanupTimeoutMs: 250,
    groupTabInto: async () => {
      throw new Error('group failed')
    },
    unmarkOwned: (tabId) => {
      calls.push(['unmarkOwned', tabId])
      return stalled()
    },
    isolateTab: (tabId) => {
      calls.push(['isolate', tabId])
      return stalled()
    },
    removeTab: (tabId) => {
      calls.push(['remove', tabId])
      return stalled()
    },
    activateTab: (tabId) => {
      calls.push(['activate', tabId])
      return stalled()
    },
    getWindow: stalled,
    focusWindow: (windowId) => {
      calls.push(['focusWindow', windowId])
      return stalled()
    },
  })

  await assert.rejects(duplicateTab(params, deps), /group failed/)

  for (const operation of ['unmarkOwned', 'isolate', 'remove', 'activate', 'focusWindow']) {
    assert.ok(calls.some((call) => call[0] === operation), `${operation} was not attempted`)
  }
})

test('failed rollback does not commit the duplicate transaction', async () => {
  const { calls, deps } = fixture({
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

  await assert.rejects(duplicateTab(params, deps), /restore failed/)
  assert.ok(!calls.some((call) => call[0] === 'complete'))
})

test('rollback isolates the duplicate before a stalled removal', { timeout: 200 }, async () => {
  const { calls, deps } = fixture({
    cleanupTimeoutMs: 5,
    groupTabInto: async () => {
      throw new Error('group failed')
    },
    removeTab: () => new Promise(() => {}),
  })

  await assert.rejects(duplicateTab(params, deps), /group failed/)
  assert.ok(calls.some((call) => call[0] === 'isolate' && call[1] === 22))
})

test('rollback focuses the previous window after tab activation rejects', async () => {
  const original = new Error('setup failed')
  const { calls, deps } = fixture({
    attachTab: async () => Promise.reject(original),
    activateTab: async (tabId) => {
      calls.push(['activate', tabId])
      throw new Error('activate failed')
    },
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  })

  await assert.rejects(duplicateTab(params, deps), (error) => error === original)
  assert.ok(calls.some((call) => call[0] === 'focusWindow'))
})

for (const [stage, failure] of [
  ['group', { groupTabInto: async () => Promise.reject(new Error('group failed')) }],
  ['attach', { attachTab: async () => Promise.reject(new Error('attach failed')) }],
  ['restore', { activateTab: async () => Promise.reject(new Error('restore failed')) }],
]) {
  test(`${stage} failure removes the orphaned duplicate`, async () => {
    const { calls, deps } = fixture(failure)
    await assert.rejects(duplicateTab(params, deps), new RegExp(`${stage} failed`))
    assert.ok(calls.some((call) => call[0] === 'unmarkOwned' && call[1] === 22))
    assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
  })
}

// The three inspection reads are bounded by the transaction deadline, but a read
// that fails must still degrade the way it did before #342 rather than reject
// the whole duplicate.
test('no focused window falls back to the source window instead of failing', async () => {
  const { calls, deps } = fixture({
    getLastFocusedWindow: async () => {
      throw new Error('no window has focus')
    },
  })
  const result = await duplicateTab(params, deps)
  assert.equal(result.targetId, 'duplicate-target')
  assert.ok(calls.some(([c]) => c === 'attach'), 'the duplicate went through')
})

test('no active tab restores the source tab instead of failing', async () => {
  const { calls, deps } = fixture({
    getActiveTabs: async () => {
      throw new Error('tabs.query failed')
    },
  })
  const result = await duplicateTab(params, deps)
  assert.equal(result.targetId, 'duplicate-target')
  assert.deepEqual(calls.find(([c]) => c === 'activate'), ['activate', 11])
})

test('a source tab that cannot be read is reported as not eligible', async () => {
  const { calls, deps } = fixture({
    getTab: async () => {
      throw new Error('No tab with id: 11')
    },
  })
  await assert.rejects(duplicateTab(params, deps), /not eligible/)
  assert.deepEqual(calls, [], 'nothing was duplicated')
})

// Once the transaction deadline has passed, no later stage may start. A 0ms
// `withTimeout` does not enforce that — an operation settling in a microtask
// beats the 0ms timer — so without the explicit guard every side-effecting stage
// (own, group, attach, activate) ran after the deadline and the duplicate
// "succeeded" late.
test('stages do not run once the transaction deadline has passed', async () => {
  const { calls, deps } = fixture({
    transactionTimeoutMs: 20,
    getActiveTabs: async () => {
      await wait(40)
      return [{ id: 7 }]
    },
  })
  await assert.rejects(duplicateTab(params, deps), /timed out/)
  // None of the stages that build the duplicate ran...
  for (const stage of ['markOwned', 'group', 'attach', 'complete']) {
    assert.ok(!calls.some(([c]) => c === stage), `${stage} must not run after the deadline`)
  }
  // ...and the rollback still did its job: the foreground went back (to the
  // source tab, since reading the active tab timed out) and the duplicate that
  // Chrome had already made was removed.
  await waitUntil(() => calls.some(([c]) => c === 'remove'))
  assert.deepEqual(calls.find(([c]) => c === 'activate'), ['activate', 11])
  assert.deepEqual(calls.find(([c]) => c === 'remove'), ['remove', 22])
})
