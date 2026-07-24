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
  await wait(20)

  assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
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
    await wait(20)

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
  await wait(20)

  assert.ok(calls.some((call) => call[0] === 'attachActive' && call[2] === false))
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
